//! Scratch: scan every FFX for renderer actions and long-trail resources.
use acvd_formats::ffx::{self, ParamList, Value};
use acvd_formats::{bnd3, vfs};
use std::collections::BTreeSet;

fn walk(list: &ParamList, acts: &mut BTreeSet<i32>, nodes: &mut BTreeSet<i32>) {
    for p in &list.params {
        if let Value::Node(id, inner) = &p.value {
            if p.kind == 38 {
                acts.insert(*id);
            }
            if p.kind == 37 {
                nodes.insert(*id);
            }
            walk(inner, acts, nodes);
        }
    }
}

fn main() -> anyhow::Result<()> {
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    let asset = "sfx/acv_commoneffects.ffxbnd";
    let data = vfs::open(&disc, asset)?;
    let binder = bnd3::read(&data)?;
    let mut hits = 0;
    for (i, e) in binder.entries.iter().enumerate() {
        let name = e.name.clone().unwrap_or_else(|| format!("#{i}"));
        if !name.starts_with("f") || !name.to_ascii_lowercase().ends_with(".ffx") {
            continue;
        }
        let Ok(bytes) = e.contents(&data) else {
            continue;
        };
        let Ok(effect) = ffx::read(&bytes) else {
            continue;
        };
        let mut acts = BTreeSet::new();
        let mut nodes = BTreeSet::new();
        for s in &effect.states {
            for a in &s.actions {
                acts.insert(a.id);
                walk(&a.params, &mut acts, &mut nodes);
            }
        }
        let res = &effect.resources;
        let trail_tex = [5000, 5009].iter().any(|t| res[1].contains(t));
        let jet_model = res[0].iter().any(|m| (1000..=1100).contains(m));
        if acts.contains(&40) || acts.contains(&108) || trail_tex || jet_model {
            hits += 1;
            println!(
                "{} id={} states={} acts={:?} nodes={:?} models={:?} tex={:?}",
                name,
                effect.id,
                effect.states.len(),
                acts.iter().collect::<Vec<_>>(),
                nodes.iter().collect::<Vec<_>>(),
                res[0],
                res[1]
            );
        }
    }
    eprintln!("{hits} hits");
    Ok(())
}
