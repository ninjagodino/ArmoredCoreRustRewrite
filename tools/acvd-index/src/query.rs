//! `acvd-index q`: print only the index rows a question needs.

use std::path::Path;

use anyhow::{bail, Result};

use crate::image::{containing, Image};

pub const USAGE: &str = "q func ADDR | callers ADDR | callees FUNC | slot TABLE OFF | ptr ADDR | field OFF [FUNC] | \
const ADDR|TEXT [FUNC] | imm VALUE [FUNC] | switch FUNC|SITE | vcall SLOT [FUNC] | name TEXT  [--limit N]";

pub fn parse_addr(s: &str) -> Option<u32> {
    let s = s.trim().trim_start_matches("FUN_").trim_start_matches('+');
    let h = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u32::from_str_radix(h, 16).ok()
}

fn same(cell: &str, want: u32) -> bool {
    parse_addr(cell) == Some(want)
}

struct Table<'a> {
    dir: &'a Path,
    limit: usize,
}

impl Table<'_> {
    fn print(&self, file: &str, keep: impl Fn(&csv::StringRecord) -> bool) -> Result<usize> {
        let mut rd = csv::Reader::from_path(self.dir.join(file))?;
        println!("{}: {}", file, rd.headers()?.iter().collect::<Vec<_>>().join(","));
        let mut n = 0;
        for row in rd.records() {
            let row = row?;
            if keep(&row) {
                if n < self.limit {
                    println!("  {}", row.iter().collect::<Vec<_>>().join(","));
                }
                n += 1;
            }
        }
        if n > self.limit {
            println!("  ... {} rows in all (--limit {})", n, self.limit);
        }
        Ok(n)
    }
}

/// The function containing `a`, read from `functions.csv`.
pub fn load_funcs(dir: &Path) -> Result<Vec<(u32, u32)>> {
    let mut rd = csv::Reader::from_path(dir.join("functions.csv"))?;
    let mut v = Vec::new();
    for row in rd.records() {
        let row = row?;
        if let (Some(s), Some(e)) = (parse_addr(&row[0]), parse_addr(&row[1])) {
            v.push((s, e));
        }
    }
    Ok(v)
}

pub fn run(img: &Image, dir: &Path, args: &[String]) -> Result<()> {
    let mut args = args.to_vec();
    let mut limit = 200;
    if let Some(i) = args.iter().position(|a| a == "--limit") {
        limit = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(limit);
        args.drain(i..(i + 2).min(args.len()));
    }
    let t = Table { dir, limit };
    let a = |i: usize| args.get(i).and_then(|s| parse_addr(s));
    let func_of = |i: usize| -> Result<Option<u32>> {
        Ok(match a(i) {
            Some(x) => {
                let f = load_funcs(dir)?;
                containing(&f, x).map(|k| f[k].0).or(Some(x))
            }
            None => None,
        })
    };
    let in_func = |row: &csv::StringRecord, f: Option<u32>| f.is_none_or(|f| same(&row[0], f));
    match args.first().map(String::as_str) {
        Some("func") => {
            let Some(x) = a(1) else { bail!(USAGE) };
            let f = load_funcs(dir)?;
            match containing(&f, x) {
                Some(k) => println!("{x:#x} is in {:#x}..{:#x} (+{:#x})", f[k].0, f[k].1, x - f[k].0),
                None => match img.section(x) {
                    Some(s) => println!("{x:#x}: no function, section {}", s.name),
                    None => println!("{x:#x}: outside the image"),
                },
            }
        }
        Some("callers") => {
            let Some(x) = a(1) else { bail!(USAGE) };
            t.print("calls.csv", |r| same(&r[2], x))?;
            t.print("slots.csv", |r| same(&r[1], x))?;
            t.print("consts.csv", |r| same(&r[3], x))?;
        }
        Some("callees") => {
            let f = func_of(1)?;
            t.print("calls.csv", |r| in_func(r, f))?;
        }
        Some("slot") => {
            let (Some(tab), Some(off)) = (a(1), a(2)) else { bail!(USAGE) };
            let v = img.word(tab + off);
            let f = load_funcs(dir)?;
            let what = if f.binary_search_by_key(&v, |x| x.0).is_ok() { "function start" } else { "not a function start" };
            println!("{tab:#x} +{off:#x} = {v:#x} ({what})");
        }
        Some("ptr") => {
            let Some(x) = a(1) else { bail!(USAGE) };
            t.print("slots.csv", |r| same(&r[1], x))?;
        }
        Some("field") => {
            let Some(off) = a(1) else { bail!(USAGE) };
            let f = func_of(2)?;
            t.print("fields.csv", |r| in_func(r, f) && same(&r[3], off))?;
        }
        Some("const") => {
            let Some(q) = args.get(1).cloned() else { bail!(USAGE) };
            let f = func_of(2)?;
            let addr = parse_addr(&q).filter(|_| q.starts_with("0x"));
            t.print("consts.csv", |r| in_func(r, f) && (addr.is_some_and(|x| same(&r[3], x)) || (addr.is_none() && r[4].contains(q.as_str()))))?;
        }
        Some("imm") => {
            let Some(q) = args.get(1).cloned() else { bail!(USAGE) };
            let v = if let Some(h) = q.strip_prefix("0x") { i64::from_str_radix(h, 16)? } else { q.parse()? };
            let f = func_of(2)?;
            t.print("imms.csv", |r| in_func(r, f) && r[3].parse::<i64>().ok() == Some(v))?;
        }
        Some("switch") => {
            let Some(x) = a(1) else { bail!(USAGE) };
            let f = func_of(1)?;
            t.print("switches.csv", |r| same(&r[1], x) || in_func(r, f))?;
            t.print("switch_cases.csv", |r| same(&r[0], x))?;
        }
        Some("vcall") => {
            let Some(sl) = a(1) else { bail!(USAGE) };
            let f = func_of(2)?;
            t.print("vcalls.csv", |r| in_func(r, f) && same(&r[2], sl))?;
        }
        Some("name") => {
            let Some(q) = args.get(1).map(|s| s.to_lowercase()) else { bail!(USAGE) };
            t.print("names.csv", |r| r.iter().any(|c| c.to_lowercase().contains(&q)))?;
        }
        _ => bail!(USAGE),
    }
    Ok(())
}
