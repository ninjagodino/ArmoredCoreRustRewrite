//! Armored Core V (base game) map survey: `acv_maps [--disc PATH] [--dump DIR]`.
//!
//! ACV ships its map layouts in mission binders (`bind/mission/mission_XXXX.bnd`,
//! `bind/mission/BattleFeild_XXXX.bnd`); each binder holds a `mission/*.xml` `<AreaList>` that
//! names the map MSBs (`<Chr>`, `<Map>`), the `ch_env` MSB and the `ch_nav` HNAV/HTR. Map-piece
//! FLVERs and the `{map}_m.mlb` link data are separate top-level BHD5 files under
//! `model/map/{map}/`. This tool finds both, validates every MSB with the shared reader, and
//! prints the mission -> map folder catalog.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use acvd_formats::{bnd3, msb, vfs};

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut take = |flag: &str| -> Option<String> {
        let i = args.iter().position(|a| a == flag)?;
        let v = args.get(i + 1).cloned();
        args.drain(i..(i + 2).min(args.len()));
        v
    };
    let dump = take("--dump").map(PathBuf::from);
    let disc_path = take("--disc")
        .map(PathBuf::from)
        .unwrap_or_else(|| vfs::repo_root().join(vfs::X360_ISO_V));
    let disc = vfs::Disc::open(&disc_path)?;

    let mut binders = Vec::new();
    for i in 0..10_000 {
        for stem in [format!("mission_{i:04}"), format!("BattleFeild_{i:04}")] {
            let path = format!("bind/mission/{stem}.bnd");
            if disc.exists(&path) {
                binders.push(path);
            }
        }
    }
    println!(
        "{} mission binders on {}",
        binders.len(),
        disc_path.display()
    );

    let mut folder_missions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut folder_msbs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut folder_models: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut envs: BTreeSet<String> = BTreeSet::new();
    let mut env_msbs: BTreeSet<String> = BTreeSet::new();
    let mut navs: BTreeSet<String> = BTreeSet::new();
    let mut msb_paths: BTreeSet<String> = BTreeSet::new();
    let mut parsed_msb: BTreeSet<String> = BTreeSet::new();
    let mut part_kinds: BTreeMap<u32, usize> = BTreeMap::new();
    let mut point_kinds: BTreeMap<u32, usize> = BTreeMap::new();
    let (mut models, mut parts, mut points) = (0usize, 0usize, 0usize);
    let mut failures = 0usize;

    for binder_path in &binders {
        let binder = disc.asset(binder_path)?;
        let b = bnd3::read(&binder)?;
        for entry in &b.entries {
            let Some(name) = entry.name.as_deref() else {
                continue;
            };
            let member = name.replace('\\', "/");
            if member.to_ascii_lowercase().ends_with(".xml") {
                let xml = String::from_utf8_lossy(&entry.contents(&binder)?).into_owned();
                let mission = format!("{binder_path}|{member}");
                for key in ["<Chr>", "<Map>", "<Env>", "<Nav>"] {
                    for rest in xml.split(key).skip(1) {
                        let Some(value) = rest.split('<').next() else {
                            continue;
                        };
                        let Some(path) = value.trim().strip_prefix("$(MapModelData)/") else {
                            continue;
                        };
                        let folder = path.split('/').next().unwrap_or_default();
                        if key == "<Chr>" || key == "<Map>" {
                            if !folder.is_empty() {
                                folder_missions
                                    .entry(folder.to_string())
                                    .or_default()
                                    .insert(mission.clone());
                            }
                        } else if key == "<Env>" {
                            envs.insert(path.to_string());
                        } else {
                            navs.insert(path.to_string());
                        }
                    }
                }
            } else if member.to_ascii_lowercase().ends_with(".msb") {
                msb_paths.insert(member.clone());
                let data = vfs::undcx(entry.contents(&binder)?.into_owned())?;
                if member.to_ascii_lowercase().contains("/ch_env/") {
                    // ACV `ch_env` files use the plain MSB magic; `env::read` accepts it and
                    // decodes type-100 lights, while the type-101 scene/fog stay open.
                    env_msbs.insert(member.clone());
                }
                if !msb::is_msb(&data) {
                    continue;
                }
                let result = (|| -> anyhow::Result<_> {
                    let m = msb::models(&data)?;
                    let p = msb::parts(&data)?;
                    let q = msb::points(&data)?;
                    Ok((m, p, q))
                })();
                match result {
                    Ok((m, p, q)) => {
                        models += m.len();
                        parts += p.len();
                        points += q.len();
                        for part in &p {
                            *part_kinds.entry(part.kind).or_default() += 1;
                        }
                        for point in &q {
                            *point_kinds.entry(point.kind).or_default() += 1;
                        }
                        if let Some(folder) = member
                            .strip_prefix("model/map/")
                            .and_then(|p| p.split('/').next())
                        {
                            folder_msbs
                                .entry(folder.to_string())
                                .or_default()
                                .insert(member.clone());
                            for part in &p {
                                if part.kind == 0 && part.model.starts_with('m') {
                                    folder_models
                                        .entry(folder.to_string())
                                        .or_default()
                                        .insert(part.model.clone());
                                }
                            }
                        }
                        parsed_msb.insert(member.clone());
                        if let Some(dir) = &dump {
                            let dest = dir.join(&member);
                            if let Some(parent) = dest.parent() {
                                std::fs::create_dir_all(parent)?;
                            }
                            std::fs::write(dest, &data)?;
                        }
                    }
                    Err(e) => {
                        failures += 1;
                        println!("FAIL {binder_path}|{member}: {e:#}");
                    }
                }
            }
        }
    }

    let mut folders: BTreeSet<String> = folder_missions.keys().cloned().collect();
    for i in 0..10_000 {
        let map = format!("m{i:04}");
        if disc.exists(&format!("model/map/{map}/{map}_m.mlb")) {
            folders.insert(map);
        }
    }
    println!(
        "{} binders, {} referenced MSBs ({} parsed, {} failures), {} env MSBs, {} env paths, {} nav meshes",
        binders.len(),
        msb_paths.len(),
        parsed_msb.len(),
        failures,
        env_msbs.len(),
        envs.len(),
        navs.len()
    );
    println!("ACV env MSBs use the plain MSB magic; env::read parses their EVENT table, BGM, type-100 light colours/angles and compact fog, and the type-101 core scene (clear/far/water/sky scale), not the type-101 fog/filter/lens flare");
    println!(
        "MSB records: {models} models, {parts} parts, {points} points; part kinds {part_kinds:?}; point kinds {point_kinds:?}"
    );
    println!(
        "{:<7} {:>5} {:>4} {:>6} {:>6} {:>6}  mlb  light",
        "map", "mis", "msb", "pieces", "flv", "l1"
    );
    for map in &folders {
        let empty = BTreeSet::new();
        let missions = folder_missions.get(map).unwrap_or(&empty);
        let msbs = folder_msbs.get(map).unwrap_or(&empty);
        let pieces = folder_models.get(map).unwrap_or(&empty);
        let full = pieces
            .iter()
            .filter(|model| disc.exists(&format!("model/map/{map}/{model}.flv")))
            .count();
        let l1 = pieces
            .iter()
            .filter(|model| disc.exists(&format!("model/map/{map}/{model}_l1.flv")))
            .count();
        println!(
            "{map:<7} {:>5} {:>4} {:>6} {full:>6} {l1:>6}  {:>3}  {:>5}",
            missions.len(),
            msbs.len(),
            pieces.len(),
            if disc.exists(&format!("model/map/{map}/{map}_m.mlb")) {
                "yes"
            } else {
                "no"
            },
            if disc.exists(&format!("model/map/{map}/{map}_l.tpf.dcx")) {
                "yes"
            } else {
                "no"
            }
        );
    }
    Ok(())
}
