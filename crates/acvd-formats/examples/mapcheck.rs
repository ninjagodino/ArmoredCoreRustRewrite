//! Parses every `.msb` (parts and points) and every `*_h.hmd` binder member of the disc:
//! `mapcheck [disc]`. Prints failures and a tally of HMD mesh counts and MSB point kinds.
//! Last run on the 360 ISO: 349 MSB (77 `ch_env/*_env.msb` are another format), 2847 HMD,
//! 92 failed, all version 0x492 enemy hit models.
use std::collections::BTreeMap;

use acvd_formats::{bnd3, hmd, msb, vfs};

fn main() -> anyhow::Result<()> {
    let root = vfs::repo_root();
    let path = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join(vfs::X360_ISO));
    let disc = vfs::Disc::open(&path)?;
    let (mut msbs, mut other_msb, mut hmds, mut failed) = (0, 0, 0, 0);
    let mut meshes: BTreeMap<usize, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<u32, usize> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut fail = |what: &str, e: anyhow::Error| {
        failed += 1;
        let reason = format!("{e:#}");
        let n = reasons.entry(reason.clone()).or_default();
        *n += 1;
        if *n <= 3 {
            println!("FAIL {what}: {reason}");
        }
    };
    for file in disc.files() {
        let lower = file.to_ascii_lowercase();
        if lower.ends_with(".msb") {
            let Ok(data) = vfs::open(&disc, &file) else {
                continue;
            };
            if !msb::is_msb(&data) {
                other_msb += 1;
                continue;
            }
            msbs += 1;
            let r = Ok(&data).and_then(|d| Ok((msb::parts(d)?, msb::points(d)?)));
            match r {
                Ok((_, points)) => points
                    .iter()
                    .for_each(|p| *kinds.entry(p.kind).or_default() += 1),
                Err(e) => fail(&file, e),
            }
        } else if lower.starts_with("model/")
            && (lower.ends_with(".bnd") || lower.ends_with(".bnd.dcx"))
        {
            let Ok(data) = vfs::open(&disc, &file) else {
                continue;
            };
            let Ok(b) = bnd3::read(&data) else { continue };
            for e in &b.entries {
                let name = e.name.as_deref().unwrap_or_default();
                if !name.to_ascii_lowercase().ends_with("_h.hmd") {
                    continue;
                }
                hmds += 1;
                let r = e
                    .contents(&data)
                    .map_err(anyhow::Error::from)
                    .and_then(|c| vfs::undcx(c.into_owned()))
                    .and_then(|d| hmd::read(&d));
                match r {
                    Ok(h) => *meshes.entry(h.meshes.len()).or_default() += 1,
                    Err(err) => fail(&format!("{file}|{name}"), err),
                }
            }
        }
    }
    println!("{msbs} msb ({other_msb} other `.msb` files skipped), {hmds} hmd, {failed} failed");
    for (reason, n) in &reasons {
        println!("{n:>6} {reason}");
    }
    println!("hmd mesh counts: {meshes:?}");
    println!("msb point kinds: {kinds:?}");
    Ok(())
}
