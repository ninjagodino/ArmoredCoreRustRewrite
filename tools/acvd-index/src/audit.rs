//! `acvd-index audit`: check every 360 address cited in `sheets/*.csv` against the index.
//!
//! Per address: a function start, an address inside a function, data that code references, or a
//! mismatch (outside the image, or code no function covers). `vtable X slot +Y = Z` claims are
//! read from the image. `+0x..` offsets and decimal constants that follow a function address
//! (up to the next address) are looked up in that function's field accesses and float loads;
//! those are reported, not failed, because the prose often means a callee or another struct.
//! PS3 citations (RPCS3, EBOOT, 01.02, TOC, `FUN_00..`) are counted as errors.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use regex::Regex;

use crate::image::{containing, Image};
use crate::query::{load_funcs, parse_addr};

struct Cite {
    file: String,
    row: String,
    addr: u32,
    offsets: Vec<u32>,
    floats: Vec<f64>,
}

pub fn run(img: &Image, root: &Path, dir: &Path) -> Result<bool> {
    let funcs = load_funcs(dir).context("run `acvd-index build` first")?;
    let starts: HashSet<u32> = funcs.iter().map(|f| f.0).collect();
    let addr_re = Regex::new(r"(?:0x|FUN_)(8[23][0-9a-fA-F]{6})\b").unwrap();
    let off_re = Regex::new(r"\+\s?0x([0-9a-fA-F]{1,4})\b").unwrap();
    let float_re = Regex::new(r"(?:^|[^0-9a-fA-Fx.+])(-?\d+\.\d+)").unwrap();
    let slot_re = Regex::new(r"vtable (0x8[23][0-9a-fA-F]{6})[^;]{0,40}?slot \+?(0x[0-9a-fA-F]+)\s*(?:=|is|\()\s*(?:FUN_|0x)?(8[23][0-9a-fA-F]{6})").unwrap();
    let ps3_re = Regex::new(r"RPCS3|EBOOT|\b01\.02\b|\bTOC\b|FUN_0[01][0-9a-fA-F]{5,6}\b").unwrap();

    let mut cites = Vec::new();
    let mut slot_claims = Vec::new();
    let mut ps3 = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(root.join("sheets"))?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "csv")).collect();
    files.sort();
    for path in &files {
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut rd = csv::ReaderBuilder::new().flexible(true).from_path(path)?;
        for row in rd.records() {
            let row = row?;
            let key = row.iter().take(2).collect::<Vec<_>>().join("/");
            for cell in row.iter() {
                if let Some(m) = ps3_re.find(cell) {
                    ps3.push((file.clone(), key.clone(), m.as_str().to_string()));
                }
                for c in slot_re.captures_iter(cell) {
                    if let (Some(t), Some(s), Some(v)) = (parse_addr(&c[1]), parse_addr(&c[2]), parse_addr(&c[3])) {
                        slot_claims.push((file.clone(), key.clone(), t, s, v));
                    }
                }
                let ms: Vec<_> = addr_re.captures_iter(cell).map(|c| (c.get(0).unwrap().range(), u32::from_str_radix(&c[1], 16).unwrap())).collect();
                for (i, (range, addr)) in ms.iter().enumerate() {
                    if *addr == crate::image::BASE {
                        continue;
                    }
                    let tail_end = ms.get(i + 1).map_or(cell.len(), |n| n.0.start);
                    let tail = &cell[range.end..tail_end];
                    let offsets = off_re.captures_iter(tail).filter_map(|c| u32::from_str_radix(&c[1], 16).ok()).collect();
                    let floats = float_re.captures_iter(tail).filter_map(|c| c[1].parse().ok()).collect();
                    cites.push(Cite { file: file.clone(), row: key.clone(), addr: *addr, offsets, floats });
                }
            }
        }
    }

    // Field offsets and float constants of the cited functions only.
    let cited: HashSet<u32> = cites.iter().filter(|c| starts.contains(&c.addr)).map(|c| c.addr).collect();
    let mut fields: HashMap<u32, HashSet<u32>> = HashMap::new();
    let mut rd = csv::Reader::from_path(dir.join("fields.csv"))?;
    for row in rd.records() {
        let row = row?;
        let Some(f) = parse_addr(&row[0]).filter(|f| cited.contains(f)) else { continue };
        let off = row[3].trim_start_matches('-');
        if let Some(o) = parse_addr(off) {
            fields.entry(f).or_default().insert(o);
        }
    }
    let mut floats: HashMap<u32, Vec<f64>> = HashMap::new();
    let mut referenced: HashSet<u32> = HashSet::new();
    let data_cited: HashSet<u32> = cites.iter().filter(|c| !img.is_code(c.addr)).map(|c| c.addr).collect();
    let mut rd = csv::Reader::from_path(dir.join("consts.csv"))?;
    for row in rd.records() {
        let row = row?;
        let t = parse_addr(&row[3]);
        if let Some(t) = t.filter(|t| data_cited.contains(t)) {
            referenced.insert(t);
        }
        let Some(f) = parse_addr(&row[0]).filter(|f| cited.contains(f)) else { continue };
        if let Ok(v) = row[4].parse::<f64>() {
            floats.entry(f).or_default().push(v);
        }
    }
    let mut rd = csv::Reader::from_path(dir.join("slots.csv"))?;
    for row in rd.records() {
        let row = row?;
        if let Some(a) = parse_addr(&row[0]).filter(|a| data_cited.contains(a)) {
            referenced.insert(a);
        }
        if let Some(t) = parse_addr(&row[2]).filter(|a| data_cited.contains(a)) {
            referenced.insert(t);
        }
    }

    let mut report = String::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut bad = Vec::new();
    let mut notes = Vec::new();
    for c in &cites {
        let kind = if starts.contains(&c.addr) {
            "function"
        } else if let Some(i) = containing(&funcs, c.addr) {
            let _ = i;
            "inside function"
        } else if img.is_code(c.addr) {
            bad.push(format!("| {} | {} | {:#x} | code outside every function |", c.file, c.row, c.addr));
            "mismatch"
        } else if img.section(c.addr).is_some() {
            if referenced.contains(&c.addr) { "data, referenced" } else { "data, no direct reference" }
        } else {
            bad.push(format!("| {} | {} | {:#x} | outside the image |", c.file, c.row, c.addr));
            "mismatch"
        };
        *counts.entry(kind).or_default() += 1;
        if kind == "function" {
            let have = fields.get(&c.addr);
            let miss: Vec<String> = c.offsets.iter().filter(|o| !have.is_some_and(|h| h.contains(o))).map(|o| format!("+{o:#x}")).collect();
            let fl = floats.get(&c.addr);
            let fmiss: Vec<String> = c
                .floats
                .iter()
                .filter(|v| !fl.is_some_and(|l| l.iter().any(|x| (x - *v).abs() <= 1e-4 * v.abs().max(1.0))))
                .map(|v| v.to_string())
                .collect();
            let found = c.offsets.len() - miss.len() + c.floats.len() - fmiss.len();
            *counts.entry("offsets/consts found in the function").or_default() += found;
            if !miss.is_empty() || !fmiss.is_empty() {
                *counts.entry("offsets/consts not in the function itself").or_default() += miss.len() + fmiss.len();
                notes.push(format!("| {} | {} | {:#x} | {} {} |", c.file, c.row, c.addr, miss.join(" "), fmiss.join(" ")));
            }
        }
    }
    let mut slot_bad = 0;
    let mut slot_lines = Vec::new();
    for (file, row, t, s, v) in &slot_claims {
        // A slot written as its own address (`slot 0x82098cb0`) rather than an offset.
        let s = &if s >= t { s - t } else { *s };
        let got = img.word(t + s);
        let ok = got == *v;
        if !ok {
            slot_bad += 1;
        }
        slot_lines.push(format!("| {file} | {row} | {t:#x} +{s:#x} | {v:#x} | {got:#x} | {} |", if ok { "ok" } else { "MISMATCH" }));
    }

    writeln!(report, "# Sheet audit against the 360 index\n")?;
    writeln!(report, "{} cited 360 addresses in {} sheets.\n", cites.len(), files.len())?;
    for (k, v) in &counts {
        writeln!(report, "- {k}: {v}")?;
    }
    writeln!(report, "- slot claims: {} ({} mismatched)", slot_claims.len(), slot_bad)?;
    writeln!(report, "- PS3 citations: {}\n", ps3.len())?;
    writeln!(report, "## Mismatches\n\n| sheet | row | address | problem |\n|---|---|---|---|")?;
    for l in &bad {
        writeln!(report, "{l}")?;
    }
    writeln!(report, "\n## Slot claims\n\n| sheet | row | table slot | claimed | image | |\n|---|---|---|---|---|---|")?;
    for l in &slot_lines {
        writeln!(report, "{l}")?;
    }
    writeln!(report, "\n## PS3 citations\n\n| sheet | row | text |\n|---|---|---|")?;
    for (f, r, t) in &ps3 {
        writeln!(report, "| {f} | {r} | `{t}` |")?;
    }
    writeln!(report, "\n## Offsets / constants not accessed by the cited function itself\n\nOften a callee, a caller's struct, or a param row offset; check by hand when a row depends on it.\n\n| sheet | row | function | not found |\n|---|---|---|---|")?;
    for l in &notes {
        writeln!(report, "{l}")?;
    }
    std::fs::write(dir.join("audit.md"), &report)?;

    println!("audit: {} cited addresses, {} mismatches, {} slot mismatches, {} PS3 citations -> {}", cites.len(), bad.len(), slot_bad, ps3.len(), dir.join("audit.md").display());
    for (k, v) in &counts {
        println!("  {k}: {v}");
    }
    Ok(bad.is_empty() && slot_bad == 0 && ps3.is_empty())
}
