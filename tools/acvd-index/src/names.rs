//! `names.csv`: field names to join against offsets and ids in the index. Sources are the
//! PARAMDEF schemas from `acvd-sheets extract` (struct, byte offset), the `.dbp` tuning labels
//! (file, field index) and the Lua param ids of `AcCtrlParamCalc.lua` (param id).
//! TDF enums wait on their reader.

use std::path::Path;

use anyhow::Result;

pub fn run(root: &Path, out_dir: &Path) -> Result<usize> {
    let mut out = csv::Writer::from_path(out_dir.join("names.csv"))?;
    out.write_record(["source", "struct", "key", "name"])?;
    let mut n = 0;

    let schema = root.join("private").join("sheets").join("schema");
    if let Ok(dir) = std::fs::read_dir(&schema) {
        for entry in dir.flatten() {
            let path = entry.path();
            let Some(st) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let mut rd = csv::ReaderBuilder::new().flexible(true).from_path(&path)?;
            let head = rd.headers()?.clone();
            let col = |name: &str| head.iter().position(|h| h == name);
            let (Some(ni), Some(oi)) = (col("name"), col("offset")) else { continue };
            for row in rd.records().flatten() {
                let (Some(name), Some(off)) = (row.get(ni), row.get(oi)) else { continue };
                let Ok(off) = off.parse::<u32>() else { continue };
                out.write_record(["paramdef", st, &format!("{off:#x}"), name])?;
                n += 1;
            }
        }
    }

    let tuning = root.join("sheets").join("tuning_fields.csv");
    if tuning.exists() {
        let mut rd = csv::Reader::from_path(&tuning)?;
        for row in rd.records().flatten() {
            if let (Some(file), Some(index), Some(ident)) = (row.get(0), row.get(1), row.get(3)) {
                out.write_record(["dbp", file, index, ident])?;
                n += 1;
            }
        }
    }

    let calc = root.join("sheets").join("ac_ctrl_calc.csv");
    if calc.exists() {
        let mut rd = csv::Reader::from_path(&calc)?;
        for row in rd.records().flatten() {
            if let (Some(name), Some(param)) = (row.get(0), row.get(1)) {
                if param != "-" && !param.is_empty() {
                    out.write_record(["lua", "AcCtrlParamCalc", param, name])?;
                    n += 1;
                }
            }
        }
    }
    out.flush()?;
    Ok(n)
}
