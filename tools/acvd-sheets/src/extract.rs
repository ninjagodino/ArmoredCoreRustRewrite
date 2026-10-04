//! Disc -> sheets. Reads every PARAMDEF and PARAM on the owned disc and writes
//! `private/sheets/{json,csv,schema}` plus the disc inventory.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};

use acvd_formats::layout::{self, ColumnSpec, Prim};
use acvd_formats::{param, paramdef};
use anyhow::{Context, Result};

use crate::model::*;
use crate::names::{self, DexDef, NameSpec};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            walk(&entry.path(), out)?;
        } else {
            out.push(entry.path());
        }
    }
    Ok(())
}

pub(crate) fn rel(base: &Path, p: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

/// Lowercase extension; DCX containers keep their inner extension (`.tpf.dcx`).
pub(crate) fn ext_of(p: &Path) -> String {
    let ext = |p: &Path| p.extension().map(|e| format!(".{}", e.to_string_lossy().to_ascii_lowercase())).unwrap_or_default();
    let outer = ext(p);
    if outer.is_empty() {
        return "(none)".into();
    }
    if outer == ".dcx" {
        if let Some(stem) = p.file_stem() {
            return format!("{}{outer}", ext(Path::new(stem)));
        }
    }
    outer
}

pub(crate) fn magic_of(head: &[u8]) -> String {
    head.iter().take(4).map(|&c| if c.is_ascii_graphic() { c as char } else { '.' }).collect()
}

/// `foo (2).bin` -> `foo.bin`, for the duplicate copies present in the dump.
pub(crate) fn dup_original(p: &Path) -> Option<PathBuf> {
    let stem = p.file_stem()?.to_str()?;
    let base = stem.strip_suffix(')')?.rsplit_once(" (")?.0;
    Some(p.with_file_name(format!("{base}{}", p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default())))
}

const PARAMDEX_GAMES: [&str; 3] = ["ACVD", "ACV", "ACFA"];

struct DefEntry {
    file: String,
    stem: String,
    def: paramdef::ParamDef,
}

pub fn run(paths: &Paths, disc: &Path) -> Result<()> {
    let usrdir = acvd_formats::vfs::usrdir(disc);
    println!("extract: scanning {}", usrdir.display());
    let mut files = Vec::new();
    walk(&usrdir, &mut files)?;
    files.sort();

    let rules: Vec<GroupRule> = read_csv(&paths.sheets().join("groups.csv"))?;
    let overrides: Vec<ColumnOverride> = read_csv(&paths.sheets().join("column_overrides.csv"))?;
    let aliases: Vec<TypeAlias> = read_csv(&paths.sheets().join("type_aliases.csv"))?;
    let excluded: Vec<ExcludedFile> = read_csv(&paths.sheets().join("excluded_files.csv"))?;
    let mut dex = HashMap::new();
    for game in PARAMDEX_GAMES {
        names::load_paramdex(&mut dex, game, &paths.paramdex(game))?;
    }
    if dex.is_empty() {
        println!("extract: no Paramdex snapshot under external/paramdex (run tools/fetch-paramdex.ps1)");
    }

    let mut inv = Inventory::default();
    let mut by_ext: BTreeMap<String, (usize, u64, HashMap<String, usize>)> = BTreeMap::new();
    let mut defs: HashMap<String, Vec<DefEntry>> = HashMap::new();
    let mut params: Vec<(PathBuf, Vec<u8>)> = Vec::new();

    for path in &files {
        let ext = ext_of(path);
        let len = std::fs::metadata(path)?.len();
        let mut head = [0u8; 64];
        let n = std::fs::File::open(path)?.read(&mut head)?;
        let slot = by_ext.entry(ext.clone()).or_default();
        slot.0 += 1;
        slot.1 += len;
        *slot.2.entry(magic_of(&head[..n])).or_default() += 1;

        if ext == ".def" {
            let data = std::fs::read(path)?;
            match paramdef::read(&data) {
                Ok(def) => defs.entry(def.param_type.clone()).or_default().push(DefEntry {
                    file: rel(&usrdir, path),
                    stem: path.file_stem().unwrap().to_string_lossy().to_ascii_lowercase(),
                    def,
                }),
                Err(e) => inv.def_errors.push(format!("{}: {e:#}", rel(&usrdir, path))),
            }
        } else if param::looks_like_param(&head[..n]) {
            let file = rel(&usrdir, path);
            if excluded.iter().any(|x| x.file.eq_ignore_ascii_case(&file)) {
                inv.excluded.push(file);
                continue;
            }
            let data = std::fs::read(path)?;
            if let Some(orig) = dup_original(path).filter(|o| o.is_file()) {
                let identical = std::fs::read(&orig)? == data;
                inv.duplicates.push(Duplicate { file: rel(&usrdir, path), original: rel(&usrdir, &orig), identical });
                if identical {
                    continue;
                }
            }
            params.push((path.clone(), data));
        }
    }

    inv.extensions = by_ext
        .into_iter()
        .map(|(extension, (files, bytes, magics))| {
            let mut magics: Vec<_> = magics.into_iter().collect();
            magics.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            magics.truncate(6);
            ExtensionCount { extension, files, bytes, magics }
        })
        .collect();

    let def_type_of = |ty: &str| -> String {
        aliases.iter().find(|a| a.param_type == ty).map(|a| a.def_type.clone()).unwrap_or_else(|| ty.to_owned())
    };

    let mut groups: BTreeMap<String, Vec<TableSheet>> = BTreeMap::new();
    let mut by_type: BTreeMap<String, Vec<(PathBuf, Vec<u8>)>> = BTreeMap::new();
    for (path, data) in params {
        let ty = param::read(&data).map(|p| p.param_type).unwrap_or_else(|_| "<unreadable>".into());
        by_type.entry(ty).or_default().push((path, data));
    }

    let used_defs: std::collections::HashSet<String> = by_type.keys().map(|t| def_type_of(t)).collect();
    for (ty, entries) in &defs {
        if !used_defs.contains(ty) {
            inv.unused_defs.extend(entries.iter().map(|d| d.file.clone()));
        }
    }

    let mut total_rows = 0usize;
    for (ty, files) in by_type {
        let Some(def_entry) = defs.get(&def_type_of(&ty)).and_then(|v| v.first()) else {
            inv.orphan_params.extend(files.iter().map(|(p, _)| format!("{} ({ty})", rel(&usrdir, p))));
            continue;
        };
        let group = rules
            .iter()
            .find(|r| def_entry.stem.starts_with(&r.def_prefix.to_ascii_lowercase()))
            .map(|r| r.group.clone())
            .unwrap_or_else(|| "_unassigned".into());
        let candidates: Vec<&DexDef> = [ty.clone(), def_entry.def.param_type.clone()]
            .iter()
            .flat_map(|t| dex.get(t).into_iter().flatten())
            .collect();
        let mut table = build_table(&ty, def_entry, &candidates, &overrides);
        let specs = specs_of(&table);

        for (path, data) in &files {
            let inst = decode_instance(&usrdir, path, data, specs.as_ref());
            total_rows += inst.rows.len();
            table.instances.push(inst);
        }
        write_csvs(paths, &group, &table)?;
        groups.entry(group).or_default().push(table);
    }

    let json_dir = paths.json();
    if json_dir.is_dir() {
        std::fs::remove_dir_all(&json_dir)?;
    }
    for (group, tables) in &groups {
        write_json(&json_dir.join(format!("{group}.json")), &GroupSheet { group: group.clone(), tables: tables.clone() })?;
    }
    write_json(&json_dir.join("_inventory.json"), &inv)?;

    let tables: usize = groups.values().map(Vec::len).sum();
    println!(
        "extract: {} files, {} defs, {tables} tables in {} groups, {total_rows} rows, {} orphan params, {} def errors",
        files.len(),
        defs.values().map(Vec::len).sum::<usize>(),
        groups.len(),
        inv.orphan_params.len(),
        inv.def_errors.len()
    );
    let a = crate::archives::run(paths, &usrdir, &files)?;
    println!(
        "extract: {} DCX files, {} binders, {} binder entries, {} textures, {} models, {} motion clips",
        a.dcx, a.binders, a.entries, a.textures, a.models, a.clips
    );
    let parts = ac_parts(paths, &usrdir)?;
    println!("extract: {} AC part records{}", parts.records.len(), parts.error.map(|e| format!(" ({e})")).unwrap_or_default());
    let tuning = tuning(paths, &usrdir)?;
    println!(
        "extract: {} tuning files, {} fields{}",
        tuning.files.len(),
        tuning.files.iter().map(|f| f.fields.len()).sum::<usize>(),
        tuning.error.map(|e| format!(" ({e})")).unwrap_or_default()
    );
    Ok(())
}

const PARAM_LIST: &str = "system/paramlist.xml";

/// Text of every `<tag>` element in `xml`, in order.
fn elements<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    xml.split(&open).skip(1).filter_map(|s| s.split_once(&close).map(|(text, _)| text.trim())).collect()
}

/// Every `<Resource>` of `system/paramlist.xml` with a `<Dbp>`: the debug-menu layout and the
/// tuning binary it describes, decoded together.
fn tuning(paths: &Paths, usrdir: &Path) -> Result<TuningSheet> {
    let roots: Vec<TuningRootRow> = read_csv(&paths.sheets().join("tuning_roots.csv"))?;
    let mut sheet = TuningSheet { source: PARAM_LIST.into(), ..Default::default() };
    let raw = std::fs::read(usrdir.join(PARAM_LIST))?;
    let Some(body) = raw.strip_prefix(&[0xff, 0xfe]) else {
        sheet.error = Some("not UTF-16LE with a byte-order mark".into());
        write_json(&paths.tuning(), &sheet)?;
        return Ok(sheet);
    };
    let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let xml = String::from_utf16_lossy(&units);
    for resource in elements(&xml, "Resource") {
        let (Some(bin), Some(dbp)) = (elements(resource, "Bin").first().copied(), elements(resource, "Dbp").first().copied()) else { continue };
        let mut file = TuningFile {
            name: bin.rsplit('\\').next().unwrap_or(bin).split('.').next().unwrap_or_default().to_owned(),
            desc: elements(resource, "Desc").first().copied().unwrap_or_default().to_owned(),
            ..Default::default()
        };
        let mut resolve = |p: &str| -> Option<String> {
            let (var, rest) = p.split_once('\\')?;
            let root = roots.iter().find(|r| r.variable == var)?;
            if !file.roots.contains(&root.variable) {
                file.roots.push(root.variable.clone());
            }
            let dir = root.directory.trim_start_matches('.').trim_matches('/');
            let rel = rest.replace('\\', "/").to_ascii_lowercase();
            Some(if dir.is_empty() { rel } else { format!("{dir}/{rel}") })
        };
        let (bin_path, dbp_path) = (resolve(bin), resolve(dbp));
        file.bin = bin_path.clone().unwrap_or_else(|| bin.to_owned());
        file.dbp = dbp_path.clone().unwrap_or_else(|| dbp.to_owned());
        let decoded = (|| -> Result<()> {
            let (Some(bin_path), Some(dbp_path)) = (bin_path, dbp_path) else { anyhow::bail!("a path variable has no tuning_roots.csv row") };
            let layout = acvd_formats::dbp::read(&std::fs::read(usrdir.join(&dbp_path)).with_context(|| dbp_path.clone())?)?;
            let data = std::fs::read(usrdir.join(&bin_path)).with_context(|| bin_path.clone())?;
            let values = layout.values(&data)?;
            let tail = &data[layout.bin_size()..];
            file.tail = tail.len();
            file.tail_nonzero = tail.iter().any(|&b| b != 0);
            file.extra_strings = layout.extra_strings;
            file.fields = layout
                .fields
                .into_iter()
                .zip(values)
                .map(|(f, value)| TuningValue {
                    kind: f.kind.name().into(),
                    label: f.label,
                    format: f.format,
                    value,
                    menu_value: f.value,
                    step: f.step,
                    min: f.min,
                    max: f.max,
                })
                .collect();
            Ok(())
        })();
        if let Err(e) = decoded {
            file.error = Some(format!("{e:#}"));
        }
        sheet.files.push(file);
    }
    write_json(&paths.tuning(), &sheet)?;
    Ok(sheet)
}

const AC_PARTS_FILE: &str = "param/acvparts.bin";

fn ac_parts(paths: &Paths, usrdir: &Path) -> Result<AcPartsSheet> {
    let categories: Vec<AcPartCategoryRow> = read_csv(&paths.sheets().join("ac_part_categories.csv"))?;
    let fields: Vec<AcPartFieldRow> = read_csv(&paths.sheets().join("ac_part_fields.csv"))?;
    let data = std::fs::read(usrdir.join(AC_PARTS_FILE))?;
    let size = |c: usize| categories.iter().find(|r| r.category == c).map(|r| r.record_size);
    let mut sheet = AcPartsSheet { path: AC_PARTS_FILE.into(), size: data.len(), ..Default::default() };
    match acvd_formats::acv_parts::read(&data, size) {
        Ok(p) => {
            sheet.counts = p.counts.to_vec();
            sheet.end = p.end;
            sheet.tail = p.tail;
            sheet.records = p
                .records
                .iter()
                .map(|r| {
                    let record = size(r.category).and_then(|n| data.get(r.offset..r.offset + n)).unwrap_or_default();
                    let fields = fields.iter().filter(|f| f.category == r.category).filter_map(|f| Some((f.name.clone(), f.read(record)?))).collect();
                    AcPartRow { category: r.category, index: r.index, offset: r.offset, id: r.id, model_id: r.model_id, fields }
                })
                .collect();
        }
        Err(e) => sheet.error = Some(format!("{e:#}")),
    }
    write_json(&paths.ac_parts(), &sheet)?;
    Ok(sheet)
}

fn dex_type_matches(dex_ty: &str, disc_ty: &str) -> bool {
    dex_ty == disc_ty || (dex_ty.starts_with("dummy8") && disc_ty == "dummy8")
}

/// First Paramdex def (ACVD, then ACV, then ACFA) whose field count and every field type equal the disc def.
fn pick_dex<'a>(def: &paramdef::ParamDef, candidates: &[&'a DexDef]) -> (Option<&'a DexDef>, Option<String>) {
    let fits = |d: &DexDef| {
        d.fields.len() == def.fields.len() && d.fields.iter().zip(&def.fields).all(|(x, f)| dex_type_matches(&x.ty, &f.display_type))
    };
    match candidates.iter().find(|d| fits(d)) {
        Some(d) => (Some(*d), None),
        None if candidates.is_empty() => (None, None),
        None => {
            let why = candidates
                .iter()
                .map(|d| format!("{} ({} fields)", d.source, d.fields.len()))
                .collect::<Vec<_>>()
                .join(", ");
            (None, Some(format!("no Paramdex def matches the disc layout ({} fields): {why}", def.fields.len())))
        }
    }
}

fn build_table(ty: &str, entry: &DefEntry, candidates: &[&DexDef], overrides: &[ColumnOverride]) -> TableSheet {
    let def = &entry.def;
    let (dex, dex_mismatch) = pick_dex(def, candidates);
    let dex_fields = dex.map(|d| &d.fields);

    let mut columns: Vec<Column> = def
        .fields
        .iter()
        .enumerate()
        .filter(|(i, _)| !overrides.iter().any(|o| o.param_type == ty && o.index == *i && o.drop))
        .map(|(i, f)| {
            let prim = Prim::parse(&f.display_type);
            let ov = overrides.iter().find(|o| o.param_type == ty && o.index == i && !o.name.is_empty());
            let disc = f.internal_name.as_deref().and_then(names::parse_name);
            let dexf = dex_fields.and_then(|d| d.get(i));
            let (spec, source): (Option<NameSpec>, Option<&str>) = if let Some(o) = ov {
                (names::parse_name(&o.name).map(|mut s| {
                    s.bits = o.bits.or(s.bits);
                    s
                }), Some("sheet"))
            } else if let Some(d) = disc.clone() {
                (Some(d), Some("disc"))
            } else if let Some(d) = dexf {
                (Some(d.spec.clone()), dex.map(|d| d.source.as_str()))
            } else {
                (None, None)
            };
            let bits = spec.as_ref().and_then(|s| s.bits);

            let mut problem = None;
            let count = match prim {
                None => {
                    problem = Some(format!("unknown display type `{}`", f.display_type));
                    0
                }
                Some(_) if bits.is_some() => 1,
                Some(p) => {
                    let by_bytes = (f.byte_count.max(0) as usize) / p.size();
                    if !(f.byte_count as usize).is_multiple_of(p.size()) {
                        problem = Some(format!("byte_count {} not a multiple of {}", f.byte_count, p.name()));
                    }
                    if let Some(c) = spec.as_ref().and_then(|s| s.count).filter(|&c| c != by_bytes) {
                        problem = Some(format!("name says [{c}] but byte_count gives {by_bytes}"));
                    }
                    by_bytes
                }
            };
            Column {
                index: i,
                ident: spec.as_ref().map(|s| names::snake(&s.name)),
                name: spec.map(|s| s.name),
                name_source: source.map(str::to_owned),
                prim: prim.map(|p| p.name().to_owned()),
                count,
                bits,
                offset: None,
                bit_offset: None,
                enum_type: (f.internal_type != f.display_type && !f.internal_type.is_empty()).then(|| f.internal_type.clone()),
                display_name: f.display_name.clone(),
                display_format: f.display_format.clone(),
                default: f.default,
                min: f.min,
                max: f.max,
                description: f.description.clone(),
                problem,
            }
        })
        .collect();

    let mut table = TableSheet {
        param_type: ty.to_owned(),
        def_file: entry.file.clone(),
        def_format_version: def.format_version,
        def_data_version: def.data_version,
        row_size: None,
        layout_error: None,
        paramdex: dex.map(|d| d.source.clone()),
        paramdex_mismatch: dex_mismatch,
        columns: Vec::new(),
        instances: Vec::new(),
    };
    match specs_from(&columns).map(|s| layout::place(&s)) {
        Some(Ok((placed, size))) => {
            for (c, p) in columns.iter_mut().zip(placed) {
                c.offset = Some(p.offset);
                c.bit_offset = p.bit_offset;
            }
            table.row_size = Some(size);
        }
        Some(Err(e)) => table.layout_error = Some(format!("{e:#}")),
        None => table.layout_error = Some("a column has an unknown type".into()),
    }
    table.columns = columns;
    table
}

fn specs_from(cols: &[Column]) -> Option<Vec<ColumnSpec>> {
    cols.iter()
        .map(|c| Some(ColumnSpec { prim: Prim::parse(c.prim.as_deref()?)?, count: c.count, bits: c.bits }))
        .collect()
}

fn specs_of(t: &TableSheet) -> Option<(Vec<ColumnSpec>, Vec<layout::Placement>, usize)> {
    let specs = specs_from(&t.columns)?;
    let (placed, size) = layout::place(&specs).ok()?;
    Some((specs, placed, size))
}

fn decode_instance(
    usrdir: &Path,
    path: &Path,
    data: &[u8],
    specs: Option<&(Vec<ColumnSpec>, Vec<layout::Placement>, usize)>,
) -> Instance {
    let file = rel(usrdir, path);
    let id = file.clone();
    let p = match param::read(data) {
        Ok(p) => p,
        Err(e) => {
            return Instance { id, file, data_version: 0, flags: [0; 3], observed_stride: None, error: Some(format!("{e:#}")), rows: vec![] }
        }
    };
    let rows = p
        .rows
        .iter()
        .map(|h| {
            let decoded = specs.ok_or_else(|| anyhow::anyhow!("table layout unresolved")).and_then(|(s, placed, size)| {
                let start = h.data_offset as usize;
                let bytes = data.get(start..start + size).with_context(|| format!("row data {start:#x}+{size:#x} past end of file"))?;
                let vals = layout::decode(s, placed, bytes)?;
                let again = layout::encode(s, placed, *size, &vals)?;
                if let Some(at) = again.iter().zip(bytes).position(|(a, b)| a != b) {
                    anyhow::bail!("round-trip mismatch at row byte {at:#x}: disc {:02x}, sheet {:02x}", bytes[at], again[at]);
                }
                vals.iter().map(serde_json::to_value).collect::<Result<Vec<_>, _>>().map_err(Into::into)
            });
            match decoded {
                Ok(v) => Row { id: h.id, name: h.name.clone(), values: Some(v), error: None },
                Err(e) => Row { id: h.id, name: h.name.clone(), values: None, error: Some(format!("{e:#}")) },
            }
        })
        .collect();
    Instance { id, file, data_version: p.data_version, flags: p.flags, observed_stride: p.observed_stride(), error: None, rows }
}

pub fn cell(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Number(n) if n.is_f64() => format!("{:?}", n.as_f64().unwrap() as f32),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a.iter().map(cell).collect::<Vec<_>>().join(" "),
        other => other.to_string(),
    }
}

fn write_csvs(paths: &Paths, group: &str, t: &TableSheet) -> Result<()> {
    let schema = paths.private_sheets().join("schema");
    std::fs::create_dir_all(&schema)?;
    let mut w = csv::Writer::from_path(schema.join(format!("{}.csv", t.param_type)))?;
    w.write_record([
        "index", "name", "name_source", "prim", "count", "bits", "offset", "bit_offset", "enum_type", "display_name", "default", "min", "max",
        "description", "problem",
    ])?;
    for c in &t.columns {
        let opt = |v: Option<String>| v.unwrap_or_default();
        w.write_record([
            c.index.to_string(),
            opt(c.name.clone()),
            opt(c.name_source.clone()),
            opt(c.prim.clone()),
            c.count.to_string(),
            opt(c.bits.map(|b| b.to_string())),
            opt(c.offset.map(|b| b.to_string())),
            opt(c.bit_offset.map(|b| b.to_string())),
            opt(c.enum_type.clone()),
            c.display_name.clone(),
            format!("{:?}", c.default),
            format!("{:?}", c.min),
            format!("{:?}", c.max),
            c.description.clone(),
            opt(c.problem.clone()),
        ])?;
    }
    w.flush()?;

    let dir = paths.private_sheets().join("csv").join(group);
    std::fs::create_dir_all(&dir)?;
    for inst in &t.instances {
        let mut w = csv::Writer::from_path(dir.join(format!("{}.csv", inst.id.replace(['/', '\\'], "__"))))?;
        let header = ["id".to_owned(), "name".to_owned()]
            .into_iter()
            .chain(t.columns.iter().map(|c| c.name.clone().unwrap_or_else(|| format!("#{}", c.index))));
        w.write_record(header)?;
        for r in &inst.rows {
            let mut rec = vec![r.id.to_string(), r.name.clone()];
            match &r.values {
                Some(v) => rec.extend(v.iter().map(cell)),
                None => rec.extend(t.columns.iter().map(|_| String::new())),
            }
            w.write_record(rec)?;
        }
        w.flush()?;
    }
    Ok(())
}
