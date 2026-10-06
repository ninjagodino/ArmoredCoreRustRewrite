//! One linear pass over every function body, writing the index CSVs.
//!
//! Register tracking is per function and straight-line (branches are not followed): `lis`,
//! `addis`, `addi`, `ori` and `mr` carry known values, any other write clears the register, and
//! calls clear the volatile ones. That is the same model the earlier `x360refs.py` /
//! `x360vcall.py` helpers used, so absolute references built across a branch are missed.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use anyhow::Result;

use crate::image::{containing, Image};

const BLR: u32 = 0x4e80_0020;
const BCTR: u32 = 0x4e80_0420;
const BCTRL: u32 = 0x4e80_0421;

struct Out {
    calls: csv::Writer<std::fs::File>,
    consts: csv::Writer<std::fs::File>,
    imms: csv::Writer<std::fs::File>,
    fields: csv::Writer<std::fs::File>,
    switches: csv::Writer<std::fs::File>,
    cases: csv::Writer<std::fs::File>,
    vcalls: csv::Writer<std::fs::File>,
}

fn hex(v: u32) -> String {
    format!("{v:#x}")
}

fn simm(w: u32) -> i32 {
    (w & 0xffff) as u16 as i16 as i32
}

fn mem_op(op: u32) -> Option<(&'static str, u8, bool)> {
    // (mnemonic, width in bytes, is store)
    Some(match op {
        32 | 33 => ("lwz", 4, false),
        34 | 35 => ("lbz", 1, false),
        40 | 41 => ("lhz", 2, false),
        42 | 43 => ("lha", 2, false),
        48 | 49 => ("lfs", 4, false),
        50 | 51 => ("lfd", 8, false),
        36 | 37 => ("stw", 4, true),
        38 | 39 => ("stb", 1, true),
        44 | 45 => ("sth", 2, true),
        52 | 53 => ("stfs", 4, true),
        54 | 55 => ("stfd", 8, true),
        58 => ("ld", 8, false),
        62 => ("std", 8, true),
        _ => return None,
    })
}

pub fn run(img: &Image, out_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let w = |name: &str| csv::Writer::from_path(out_dir.join(name));

    let funcs = functions(img);
    let starts: HashSet<u32> = funcs.iter().map(|f| f.0).collect();
    let ranges: Vec<(u32, u32)> = funcs.iter().map(|f| (f.0, f.1)).collect();
    {
        let mut f = w("functions.csv")?;
        f.write_record(["start", "end", "size", "source"])?;
        for &(s, e, src) in &funcs {
            f.write_record([hex(s), hex(e), (e - s).to_string(), src.to_string()])?;
        }
        f.flush()?;
    }

    let mut out = Out {
        calls: w("calls.csv")?,
        consts: w("consts.csv")?,
        imms: w("imms.csv")?,
        fields: w("fields.csv")?,
        switches: w("switches.csv")?,
        cases: w("switch_cases.csv")?,
        vcalls: w("vcalls.csv")?,
    };
    out.calls.write_record(["caller", "site", "callee", "kind"])?;
    out.consts.write_record(["func", "site", "op", "target", "value"])?;
    out.imms.write_record(["func", "site", "op", "value"])?;
    out.fields.write_record(["func", "site", "op", "offset", "width", "access", "base"])?;
    out.switches.write_record(["func", "site", "table", "cases"])?;
    out.cases.write_record(["site", "case", "target"])?;
    out.vcalls.write_record(["func", "site", "slot"])?;

    let mut n_switch = 0usize;
    for &(s, e, _) in &funcs {
        n_switch += scan(img, s, e, &starts, &ranges, &mut out)?;
    }
    for wr in [&mut out.calls, &mut out.consts, &mut out.imms, &mut out.fields, &mut out.switches, &mut out.cases, &mut out.vcalls] {
        wr.flush()?;
    }

    let (n_ptr, n_tab) = pointer_tables(img, &starts, out_dir)?;
    println!(
        "index: {} functions ({} pdata, {} leaf), {} switch tables, {} function pointers in {} tables -> {}",
        funcs.len(),
        funcs.iter().filter(|f| f.2 == "pdata").count(),
        funcs.iter().filter(|f| f.2 == "leaf").count(),
        n_switch,
        n_ptr,
        n_tab,
        out_dir.display()
    );
    Ok(())
}

/// `.pdata` functions plus leaf functions, each running to its first `blr` or unconditional `b`:
/// direct `bl` targets outside every `.pdata` body, and data words (vtable slots, callbacks)
/// pointing outside every body at a function boundary (after padding, a `blr`, a `b`, or a body end).
pub fn functions(img: &Image) -> Vec<(u32, u32, &'static str)> {
    let pdata = &img.pdata;
    let mut leaf = BTreeSet::new();
    for &(s, e) in pdata {
        for a in (s..e).step_by(4) {
            let w = img.word(a);
            if w >> 26 == 18 && w & 3 == 1 {
                let t = branch_target(a, w);
                if img.is_code(t) && containing(pdata, t).is_none() {
                    leaf.insert(t);
                }
            }
        }
    }
    let ends: HashSet<u32> = pdata.iter().map(|f| f.1).collect();
    let at_boundary = |t: u32| {
        if t & 3 != 0 || !img.is_code(t) || img.word(t) == 0 || containing(pdata, t).is_some() {
            return false;
        }
        let prev = img.word(t - 4);
        prev == 0 || prev == BLR || (prev >> 26 == 18 && prev & 1 == 0) || ends.contains(&t)
    };
    for (lo, hi) in img.data_ranges() {
        for a in (lo..hi).step_by(4) {
            let t = img.word(a);
            if at_boundary(t) {
                leaf.insert(t);
            }
        }
    }
    // Code pointers built in registers (`lis rX,hi` then `addi rY,rX,lo`), e.g. handler tables
    // filled at run time.
    for &(s, e) in pdata {
        let mut hi: [Option<u32>; 32] = [None; 32];
        for a in (s..e).step_by(4) {
            let w = img.word(a);
            let (rd, ra, imm) = ((w >> 21 & 31) as usize, (w >> 16 & 31) as usize, w & 0xffff);
            match w >> 26 {
                15 if ra == 0 => hi[rd] = Some(imm << 16),
                14 => {
                    if let Some(h) = hi[ra] {
                        let t = h.wrapping_add(imm as i16 as u32);
                        if at_boundary(t) {
                            leaf.insert(t);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut funcs: Vec<(u32, u32, &'static str)> = pdata.iter().map(|&(s, e)| (s, e, "pdata")).collect();
    let next_pdata = |a: u32| pdata.get(pdata.partition_point(|f| f.0 <= a)).map_or(u32::MAX, |f| f.0);
    let leaf: Vec<u32> = leaf.into_iter().collect();
    for (i, &s) in leaf.iter().enumerate() {
        let limit = next_pdata(s).min(leaf.get(i + 1).copied().unwrap_or(u32::MAX)).min(s + 0x1000);
        let mut e = s;
        while e < limit {
            let w = img.word(e);
            e += 4;
            if w == BLR || (w >> 26 == 18 && w & 1 == 0) {
                break;
            }
        }
        funcs.push((s, e, "leaf"));
    }
    funcs.sort_unstable();
    funcs
}

fn branch_target(a: u32, w: u32) -> u32 {
    let li = ((w & 0x03ff_fffc) << 6) as i32 >> 6;
    if w & 2 != 0 { li as u32 } else { a.wrapping_add(li as u32) }
}

fn scan(img: &Image, s: u32, e: u32, starts: &HashSet<u32>, ranges: &[(u32, u32)], out: &mut Out) -> Result<usize> {
    let mut reg: [Option<u32>; 32] = [None; 32];
    // Register -> vtable slot it was loaded from (`lwz rX,SLOT(rY)` with rY itself a `lwz` result).
    let mut slot_of: [Option<u32>; 32] = [None; 32];
    let mut vptr: [bool; 32] = [false; 32];
    let mut switches = 0;
    let f = hex(s);
    for a in (s..e).step_by(4) {
        let w = img.word(a);
        let op = w >> 26;
        let rd = ((w >> 21) & 31) as usize;
        let ra = ((w >> 16) & 31) as usize;
        let rb = ((w >> 11) & 31) as usize;
        let site = hex(a);
        let mut wrote: Option<usize> = None;
        let mut known: Option<u32> = None;
        let mut is_vptr = false;
        let mut slot: Option<u32> = None;
        match op {
            14 | 15 => {
                let v = if op == 15 { ((w & 0xffff) << 16) as i32 } else { simm(w) };
                if ra == 0 {
                    known = Some(v as u32);
                    if op == 14 {
                        out.imms.write_record([&f, &site, "li", &v.to_string()])?;
                    }
                } else if let Some(base) = reg[ra] {
                    let t = base.wrapping_add(v as u32);
                    known = Some(t);
                    if op == 14 {
                        const_ref(img, &f, &site, "addi", t, starts, out)?;
                    }
                }
                wrote = Some(rd);
            }
            24 | 25 => {
                // ori / oris rA,rS,UIMM
                let v = if op == 24 { w & 0xffff } else { (w & 0xffff) << 16 };
                if let Some(base) = reg[rd] {
                    known = Some(base | v);
                    if op == 24 && w & 0xffff != 0 {
                        const_ref(img, &f, &site, "ori", base | v, starts, out)?;
                    }
                }
                wrote = Some(ra);
            }
            10 | 11 => {
                let name = if op == 10 { "cmplwi" } else { "cmpwi" };
                let v = if op == 10 { (w & 0xffff) as i32 } else { simm(w) };
                out.imms.write_record([&f, &site, name, &v.to_string()])?;
            }
            7 | 8 | 12 | 13 => wrote = Some(rd),
            20 | 21 | 23 | 26..=30 => wrote = Some(ra),
            18 => {
                let t = branch_target(a, w);
                if w & 1 == 1 {
                    out.calls.write_record([&f, &site, &hex(t), "call"])?;
                    clobber(&mut reg, &mut slot_of, &mut vptr);
                    continue;
                } else if !(s..e).contains(&t) && starts.contains(&t) {
                    out.calls.write_record([&f, &site, &hex(t), "tail"])?;
                }
            }
            19 => {
                if w == BCTRL {
                    clobber(&mut reg, &mut slot_of, &mut vptr);
                    continue;
                }
            }
            31 => {
                let xo = (w >> 1) & 0x3ff;
                match xo {
                    0 | 32 => {}
                    444 if rd == rb => {
                        // mr rA,rS
                        reg[ra] = reg[rd];
                        slot_of[ra] = slot_of[rd];
                        vptr[ra] = vptr[rd];
                        continue;
                    }
                    467 if (w >> 11) & 0x3ff == 0x120 => {
                        // mtctr rS: a virtual call when rS came from a vtable slot.
                        if let Some(sl) = slot_of[rd] {
                            if (a + 4..(a + 0x14).min(e)).step_by(4).any(|b| img.word(b) == BCTRL) {
                                out.vcalls.write_record([&f, &site, &hex(sl)])?;
                            }
                        }
                        if img.word(a + 4) == BCTR || img.word(a + 8) == BCTR {
                            switches += switch_at(img, &f, a, s, e, rd, &reg_lwzx(img, a, s, &reg_snapshot(img, s, a)), ranges, out)?;
                        }
                        continue;
                    }
                    _ => {
                        wrote = Some(rd);
                        reg[ra] = None;
                        slot_of[ra] = None;
                        vptr[ra] = false;
                    }
                }
            }
            _ => {}
        }
        if let Some((name, width, store)) = mem_op(op) {
            let d = if op == 58 || op == 62 { simm(w) & !3 } else { simm(w) };
            let update = matches!(op, 33 | 35 | 37 | 39 | 41 | 43 | 45 | 49 | 51 | 53 | 55) || ((op == 58 || op == 62) && w & 3 == 1);
            if let Some(base) = reg[ra].filter(|_| ra != 0) {
                const_ref(img, &f, &site, name, base.wrapping_add(d as u32), starts, out)?;
            } else if ra != 0 && ra != 1 {
                let access = if store { "w" } else { "r" };
                out.fields.write_record([&f, &site, name, &format!("{:#x}", d), &width.to_string(), access, &format!("r{ra}")])?;
                if name == "lwz" {
                    if vptr[ra] {
                        slot = Some(d as u32);
                    } else {
                        is_vptr = true;
                    }
                }
            }
            if update {
                reg[ra] = None;
                slot_of[ra] = None;
                vptr[ra] = false;
            }
            if !store && !matches!(op, 48..=51) {
                wrote = Some(rd);
            }
        }
        if op == 46 {
            for r in rd..32 {
                reg[r] = None;
                slot_of[r] = None;
                vptr[r] = false;
            }
        }
        if let Some(r) = wrote {
            reg[r] = known;
            slot_of[r] = slot;
            vptr[r] = is_vptr && slot.is_none();
        }
    }
    Ok(switches)
}

fn clobber(reg: &mut [Option<u32>; 32], slot_of: &mut [Option<u32>; 32], vptr: &mut [bool; 32]) {
    for r in [0, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] {
        reg[r] = None;
        slot_of[r] = None;
        vptr[r] = false;
    }
}

/// Register values just before `upto`, recomputed (only lis/addi/ori/mr) for the switch lookup.
fn reg_snapshot(img: &Image, s: u32, upto: u32) -> [Option<u32>; 32] {
    let mut reg: [Option<u32>; 32] = [None; 32];
    let lo = upto.saturating_sub(0x80).max(s);
    for a in (lo..upto).step_by(4) {
        let w = img.word(a);
        let (op, rd, ra) = (w >> 26, ((w >> 21) & 31) as usize, ((w >> 16) & 31) as usize);
        match op {
            15 if ra == 0 => reg[rd] = Some((w & 0xffff) << 16),
            14 | 15 => reg[rd] = reg[ra].filter(|_| ra != 0).map(|b| b.wrapping_add(if op == 15 { (w & 0xffff) << 16 } else { simm(w) as u32 })),
            24 => reg[ra] = reg[rd].map(|b| b | (w & 0xffff)),
            18 if w & 1 == 1 => reg = [None; 32],
            _ => {}
        }
    }
    reg
}

/// The table base of the `lwzx` feeding the `mtctr` at `mt`: (table, lwzx site).
fn reg_lwzx(img: &Image, mt: u32, s: u32, reg: &[Option<u32>; 32]) -> Option<(u32, u32)> {
    for a in (mt.saturating_sub(0x20).max(s)..mt).step_by(4).rev() {
        let w = img.word(a);
        if w >> 26 == 31 && (w >> 1) & 0x3ff == 23 {
            let (ra, rb) = (((w >> 16) & 31) as usize, ((w >> 11) & 31) as usize);
            return reg[ra].or(reg[rb]).map(|t| (t, a));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn switch_at(img: &Image, f: &str, mt: u32, s: u32, e: u32, _rd: usize, table: &Option<(u32, u32)>, ranges: &[(u32, u32)], out: &mut Out) -> Result<usize> {
    let Some((table, lwzx)) = *table else { return Ok(0) };
    let mut cases = None;
    for a in (lwzx.saturating_sub(0x40).max(s)..lwzx).step_by(4).rev() {
        let w = img.word(a);
        if w >> 26 == 10 && (w >> 21) & 1 == 0 {
            cases = Some((w & 0xffff) + 1);
            break;
        }
    }
    let n = cases.unwrap_or(512).min(4096);
    let own = containing(ranges, s).map(|i| ranges[i]).unwrap_or((s, e));
    let mut written = 0;
    for i in 0..n {
        let t = img.word(table + i * 4);
        if cases.is_none() && !(own.0..own.1).contains(&t) {
            break;
        }
        out.cases.write_record([hex(mt), i.to_string(), hex(t)])?;
        written += 1;
    }
    out.switches.write_record([f.to_string(), hex(mt), hex(table), written.to_string()])?;
    Ok(1)
}

fn const_ref(img: &Image, f: &str, site: &str, op: &str, t: u32, starts: &HashSet<u32>, out: &mut Out) -> Result<()> {
    if img.section(t).is_none() {
        return Ok(());
    }
    let value = if starts.contains(&t) {
        "func".to_string()
    } else {
        match op {
            "lfs" => format!("{}", f32::from_bits(img.word(t))),
            "lfd" => format!("{}", f64::from_bits((img.word(t) as u64) << 32 | img.word(t + 4) as u64)),
            "lwz" => hex(img.word(t)),
            _ => img.cstr(t).map(|s| format!("str:{s}")).unwrap_or_default(),
        }
    };
    out.consts.write_record([f, site, op, &hex(t), &value])?;
    Ok(())
}

/// Aligned data words equal to a function start; consecutive runs form the tables.
fn pointer_tables(img: &Image, starts: &HashSet<u32>, out_dir: &Path) -> Result<(usize, usize)> {
    let mut slots = csv::Writer::from_path(out_dir.join("slots.csv"))?;
    let mut tables = csv::Writer::from_path(out_dir.join("vtables.csv"))?;
    slots.write_record(["addr", "target", "table", "slot"])?;
    tables.write_record(["table", "end", "count"])?;
    let mut runs: BTreeMap<u32, u32> = BTreeMap::new();
    let mut n = 0;
    for (lo, hi) in img.data_ranges() {
        let mut run: Option<u32> = None;
        for a in (lo..hi).step_by(4) {
            let t = img.word(a);
            if starts.contains(&t) {
                let start = *run.get_or_insert(a);
                slots.write_record([hex(a), hex(t), hex(start), hex(a - start)])?;
                *runs.entry(start).or_default() += 1;
                n += 1;
            } else {
                run = None;
            }
        }
    }
    for (&t, &c) in &runs {
        tables.write_record([hex(t), hex(t + c * 4), c.to_string()])?;
    }
    slots.flush()?;
    tables.flush()?;
    Ok((n, runs.len()))
}
