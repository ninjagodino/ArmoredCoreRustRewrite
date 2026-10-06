//! Prints the node tree of FFX effects: each state's actions, then every param-37 node (template
//! id) and param-38 action (type id) nested under them, with their life / delay ticks.
//! `ffxtree [--disc PATH] ID...` (effects of `sfx/acv_commoneffects.ffxbnd`).
use acvd_formats::ffx::{self, ParamList, Value};
use acvd_formats::vfs;

fn walk(l: &ParamList, depth: usize) {
    for (i, p) in l.params.iter().enumerate() {
        if let Value::Node(id, list) = &p.value {
            let ticks: Vec<String> = list
                .params
                .iter()
                .enumerate()
                .filter_map(|(j, q)| match q.value {
                    Value::Tick(t) => Some(format!("[{j}]={t}")),
                    _ => None,
                })
                .collect();
            let tag = if p.kind == 37 { "node" } else { "action" };
            println!(
                "{}[{i}] {tag} {id} ({} params) {}",
                "  ".repeat(depth),
                list.params.len(),
                ticks.join(" ")
            );
            walk(list, depth + 1);
        }
    }
}

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let path = match args.iter().position(|a| a == "--disc") {
        Some(i) => {
            args.remove(i);
            std::path::PathBuf::from(args.remove(i))
        }
        None => vfs::repo_root().join(vfs::X360_ISO),
    };
    let disc = vfs::Disc::open(&path)?;
    for id in &args {
        let id: i32 = id.parse()?;
        let e = ffx::read(&vfs::open(&disc, &format!("sfx/acv_commoneffects.ffxbnd|f{id:07}.ffx"))?)?;
        println!("== effect {id}: {} states, resources {:?}", e.states.len(), e.resources);
        for (s, state) in e.states.iter().enumerate() {
            for a in &state.actions {
                println!("state {s} action {}", a.id);
                walk(&a.params, 1);
            }
        }
    }
    Ok(())
}
