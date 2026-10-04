//! `.ani` -> clip sheet rows: shape, track kinds and (for named clips) the skeleton; and the
//! motion set each AC part selects (`sheets/ac_motion.csv`).

use acvd_formats::ani;
use anyhow::{Context, Result};
use serde_json::Value;

use crate::model::*;

pub fn clip(name: &str, data: &[u8]) -> ClipRow {
    let mut row = ClipRow { name: name.to_owned(), named: ani::magic(data).is_some_and(|m| m != ani::UNNAMED), ..Default::default() };
    match ani::read(data) {
        Ok(a) => {
            row.frames = a.frames;
            row.bones = a.bones.len();
            row.quantized = a.quantized;
            row.kinds = ani::LAYOUTS.iter().map(|l| a.bones.iter().filter(|b| b.track.kind == l.kind).count()).collect();
            row.empty_tracks = a.bones.iter().filter(|b| b.track.keys.is_empty()).count();
            row.cameras = a.bones.iter().filter(|b| b.camera.is_some()).count();
            row.keys = a.bones.iter().map(|b| b.track.keys.len()).sum();
            row.skeleton = a.bones.iter().filter_map(|b| b.rest.as_ref().map(|r| ClipBone { name: r.name.clone(), parent: r.parent, translation: r.translation })).collect();
        }
        Err(e) => row.error = Some(format!("{e:#}")),
    }
    row
}

/// The motion set one AC part selects: the binder's asset path and its clip carrying the rest
/// skeleton, or why the chain broke (`check`, detail).
pub struct PartMotion {
    pub category: usize,
    pub id: u16,
    pub set: Result<(String, String), (&'static str, String)>,
}

/// Every record of each `ac_motion.csv` slot's category, in `acvparts.bin` order.
pub fn part_motions(paths: &Paths) -> Result<Vec<PartMotion>> {
    let rows: Vec<AcMotionRow> = read_csv(&paths.sheets().join("ac_motion.csv"))?;
    let slots: Vec<AssemblySlotRow> = read_csv(&paths.sheets().join("assembly_slots.csv"))?;
    let parts: AcPartsSheet = read_json(&paths.ac_parts())?;
    let groups = load_groups(paths)?;
    let motions = load_motion_groups(paths)?;
    let mut out = Vec::new();
    for row in &rows {
        let slot = slots.iter().find(|s| s.slot == row.slot).with_context(|| format!("ac_motion.csv: no slot `{}` in assembly_slots.csv", row.slot))?;
        let table = groups.iter().flat_map(|g| &g.tables).find(|t| t.param_type == row.table).with_context(|| format!("ac_motion.csv: no table {}", row.table))?;
        let col = |name: &str| table.columns.iter().position(|c| c.ident.as_deref() == Some(name)).with_context(|| format!("ac_motion.csv: {} has no column `{name}`", row.table));
        let (filter_col, filter_value) = row.filter.split_once('=').with_context(|| format!("ac_motion.csv: filter `{}` is not <column>=<value>", row.filter))?;
        let filter_value: i64 = filter_value.trim().parse().with_context(|| format!("ac_motion.csv: filter value `{filter_value}`"))?;
        let (key, filter, name) = (col(&row.key)?, col(filter_col.trim())?, col(&row.name_column)?);
        let rows_of = || table.instances.iter().flat_map(|i| &i.rows).filter_map(|r| r.values.as_ref());
        for p in parts.records.iter().filter(|p| p.category == slot.category) {
            let set = (|| {
                let value = *p.fields.get(&row.field).ok_or(("motion.field_missing", format!("record has no `{}` (ac_part_fields.csv)", row.field)))? as i64;
                let hit = rows_of()
                    .find(|v| v.get(key).and_then(Value::as_i64) == Some(value) && v.get(filter).and_then(Value::as_i64) == Some(filter_value))
                    .ok_or(("motion.collate_missing", format!("no {} row with {} = {value} and {}", row.table, row.key, row.filter)))?;
                let name = hit.get(name).and_then(Value::as_str).unwrap_or_default();
                let path = row.binder_path(name);
                let set = motions.iter().flat_map(|g| &g.sets).find(|s| s.path == path).ok_or(("motion.set_missing", format!("no motion binder {path}")))?;
                let clip = set.clips.iter().find(|c| !c.skeleton.is_empty()).ok_or(("motion.no_skeleton", format!("{path} has no named clip")))?;
                Ok((path, clip.name.clone()))
            })();
            out.push(PartMotion { category: p.category, id: p.id, set });
        }
    }
    Ok(out)
}
