//! Probe-helper queries used by `tools/xenia-probe`:
//! `mission <id>` (XML existence), `exists <path>`, `mapfiles <map>`, and
//! `msb <map> <model> [instance]` (authored MSB part transforms as JSON lines).
use acvd_formats::{msb, vfs};

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn open_asset(disc: &vfs::Disc, path: &str) -> Option<Vec<u8>> {
    vfs::open(disc, path).ok()
}

fn open_disc() -> anyhow::Result<vfs::Disc> {
    let root = vfs::repo_root();
    vfs::Disc::open(&vfs::default_disc(&root))
}

fn normalized_map(value: &str) -> String {
    let text = value.trim().to_ascii_lowercase();
    if text.starts_with('m') {
        text
    } else {
        format!("m{text}")
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: probe_helper mission <id> | exists <path> | mapfiles <map> | msb <map> <model> [instance]");
        std::process::exit(2);
    }
    let disc = open_disc()?;
    match args[0].as_str() {
        "mission" => {
            let id: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let path = format!("mission/ch{id:04}.xml");
            let data = open_asset(&disc, &path);
            let bytes = data.as_ref().map_or(0, |d| d.len());
            println!(
                "{{\"id\":{id},\"path\":\"{}\",\"exists\":{},\"bytes\":{bytes}}}",
                json_escape(&path),
                if data.is_some() { "true" } else { "false" }
            );
        }
        "exists" => {
            let path = args.get(1).map(String::as_str).unwrap_or("");
            let exists = open_asset(&disc, path).is_some();
            println!(
                "{{\"path\":\"{}\",\"exists\":{}}}",
                json_escape(path),
                if exists { "true" } else { "false" }
            );
        }
        "mapfiles" => {
            let map = normalized_map(args.get(1).map(String::as_str).unwrap_or(""));
            for (key, path) in [
                ("base", format!("model/map/{map}/{map}.msb")),
                ("map", format!("model/map/{map}/{map}_map.msb")),
                ("env", format!("model/map/ch_env/{map}_env.msb")),
            ] {
                let data = open_asset(&disc, &path);
                let bytes = data.as_ref().map_or(0, |d| d.len());
                println!(
                    "{{\"{key}\":{{\"path\":\"{}\",\"exists\":{},\"bytes\":{bytes}}}}}",
                    json_escape(&path),
                    if data.is_some() { "true" } else { "false" }
                );
            }
        }
        "msb" => {
            let map = normalized_map(args.get(1).map(String::as_str).unwrap_or(""));
            let model = args.get(2).map(String::as_str).unwrap_or("");
            let instance = args.get(3).map(String::as_str);
            for path in [
                format!("model/map/{map}/{map}_map.msb"),
                format!("model/map/{map}/{map}.msb"),
            ] {
                let Some(data) = open_asset(&disc, &path) else {
                    continue;
                };
                if !msb::is_msb(&data) {
                    continue;
                }
                let parts = msb::parts(&data)?;
                for (index, part) in parts.iter().enumerate() {
                    if part.model != model {
                        continue;
                    }
                    if let Some(want) = instance {
                        if want.parse::<usize>().map_or(false, |n| n != index) {
                            continue;
                        }
                    }
                    println!(
                        "{{\"source\":\"{}\",\"name\":\"{}\",\"model\":\"{}\",\"t\":[{},{},{}],\"rot\":[{},{},{}]}}",
                        json_escape(&path),
                        json_escape(&part.name),
                        json_escape(&part.model),
                        part.translation[0], part.translation[1], part.translation[2],
                        part.rotation_deg[0], part.rotation_deg[1], part.rotation_deg[2]
                    );
                }
            }
        }
        other => {
            eprintln!("unknown command: {other}");
            std::process::exit(2);
        }
    }
    Ok(())
}
