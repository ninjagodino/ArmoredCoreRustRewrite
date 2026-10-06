//! Reads every TPF on the 360 ISO (loose and inside binders), checks each BC1/BC3 size against
//! the Xenos tiled layout, and compares the untiled level 0 with the PS3 dump's copy:
//! `tpfscan`. Prints a tally of header fields, format pairings and size and block matches.
//! Last run: 10556 360 packs; every BC1/BC3 stored size matches; format codes 23/24/25 occur
//! only on the 360, each paired with a PS3 normal map.
use std::collections::BTreeMap;

use acvd_formats::{bnd3, tpf, vfs};

fn tpfs(disc: &vfs::Disc) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for file in disc.files() {
        if vfs::is_bundle(&file) {
            continue;
        }
        let Ok(raw) = disc.read(&file) else { continue };
        let Ok(data) = vfs::undcx(raw) else { continue };
        if tpf::is_tpf(&data) {
            out.push((file.to_ascii_lowercase(), data));
        } else if bnd3::is_bnd3(&data) {
            let Ok(b) = bnd3::read(&data) else { continue };
            for e in &b.entries {
                let Ok(c) = e.contents(&data) else { continue };
                let Ok(c) = vfs::undcx(c.into_owned()) else { continue };
                if tpf::is_tpf(&c) {
                    out.push((format!("{}|{}", file.to_ascii_lowercase(), e.name.clone().unwrap_or_default().to_ascii_lowercase()), c));
                }
            }
        }
    }
    out
}

fn block(fmt: u8) -> Option<u32> {
    match fmt {
        0 => Some(8),
        5 | 33 => Some(16),
        _ => None,
    }
}

fn main() -> anyhow::Result<()> {
    let root = vfs::repo_root();
    let x = vfs::Disc::open(&root.join(vfs::X360_ISO))?;
    let p = vfs::Disc::open(&root.join(vfs::PS3_DUMP))?;
    let xs = tpfs(&x);
    let ps: BTreeMap<String, Vec<u8>> = tpfs(&p).into_iter().collect();
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    let mut fails = 0;
    for (name, d) in &xs {
        let t = match tpf::read(d) {
            Ok(t) => t,
            Err(e) => {
                *tally.entry(format!("read error {e:#}")).or_default() += 1;
                if fails < 10 {
                    fails += 1;
                    println!("FAIL {name}: {e:#}");
                }
                continue;
            }
        };
        *tally.entry(format!("pack plat {} flag2 {} enc {}", t.platform, t.flag2, t.encoding)).or_default() += 1;
        if t.platform != tpf::PLATFORM_X360 {
            println!("ps3-platform pack on 360: {name}");
        }
        let pt = ps.get(name).and_then(|pd| tpf::read(pd).ok().map(|t| (t, pd)));
        for tex in &t.textures {
            *tally.entry(format!("fmt {} kind {} f1 {} unk1 {:#x} floats {}", tex.format, tex.kind, tex.flags1, tex.unk1, tex.floats.len())).or_default() += 1;
            let Some(bb) = block(tex.format) else {
                let ptex = pt.as_ref().and_then(|(pt, _)| pt.textures.iter().find(|q| q.name.eq_ignore_ascii_case(&tex.name)));
                let k = format!("new fmt {} -> ps3 {:?}", tex.format, ptex.map(|q| (q.format, q.width == tex.width && q.height == tex.height, q.size)));
                let n = tally.entry(k.split(", Some(").next().unwrap_or("").to_string() + &ptex.map_or(String::new(), |q| format!(" ps3 fmt {}", q.format))).or_default();
                *n += 1;
                if *n <= 3 {
                    println!("{k}: {name} {} {}x{} mips {} size {}", tex.name, tex.width, tex.height, tex.mipmaps, tex.size);
                }
                continue;
            };
            let want = tpf::stored_size(t.platform, tex.width, tex.height, tex.levels(), tex.faces(), bb);
            let ok = want == tex.size as u64;
            *tally.entry(format!("size ok {ok} fmt {}", tex.format)).or_default() += 1;
            if !ok && fails < 30 {
                fails += 1;
                println!("size {name} {} fmt {} {}x{} mips {} kind {} stored {} want {want}", tex.name, tex.format, tex.width, tex.height, tex.mipmaps, tex.kind, tex.size);
            }
            let Some((pt, pd)) = &pt else {
                *tally.entry("no ps3 pack".into()).or_default() += 1;
                continue;
            };
            let Some(ptex) = pt.textures.iter().find(|q| q.name.eq_ignore_ascii_case(&tex.name)) else {
                *tally.entry("no ps3 texture".into()).or_default() += 1;
                continue;
            };
            *tally.entry(format!("pair 360 {} -> ps3 {}", tex.format, ptex.format)).or_default() += 1;
            if (ptex.width, ptex.height, ptex.kind) != (tex.width, tex.height, tex.kind) || !ok {
                *tally.entry("pair differs in size".into()).or_default() += 1;
                continue;
            }
            let (Ok(a), Ok(b)) = (tex.linear(t.platform, d, bb), ptex.linear(pt.platform, pd, bb)) else { continue };
            let l0 = (tex.width as usize).div_ceil(4) * (tex.height as usize).div_ceil(4) * bb as usize;
            let pl = tpf::block_level_span(ptex.width, ptex.height, ptex.levels(), ptex.faces(), b.len() as u64, 0, 0, bb);
            let same = a[..l0] == b[pl.0 as usize..(pl.0 + pl.1) as usize];
            let close = a[..l0].chunks(bb as usize).zip(b[..l0].chunks(bb as usize)).filter(|(u, v)| u == v).count() * 100 / (l0 / bb as usize);
            *tally.entry(format!("level0 identical {same}")).or_default() += 1;
            if !same {
                *tally.entry(format!("level0 blocks equal {}%", close / 10 * 10)).or_default() += 1;
            }
        }
    }
    println!("{} 360 tpfs, {} ps3", xs.len(), ps.len());
    for (k, n) in tally {
        println!("{n:>7} {k}");
    }
    Ok(())
}
