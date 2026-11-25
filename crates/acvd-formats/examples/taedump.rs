//! Prints every timed event of one or more `.tae` files:
//! `taedump PATH...` (disc asset paths) — animation id, event kind, frame range, raw params
//! and the decoded FFX / sound ids.
use acvd_formats::tae::{self, KIND_FFX};
use acvd_formats::vfs;

fn main() -> anyhow::Result<()> {
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    for path in std::env::args().skip(1) {
        let bytes = match vfs::open(&disc, &path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{path}: {e}");
                continue;
            }
        };
        let t = tae::read(&bytes)?;
        println!(
            "== {path} (version {:#x}, file_id {}, {} anims) ==",
            t.version,
            t.file_id,
            t.animations.len()
        );
        for a in &t.animations {
            println!("  anim {:#x}: {} events", a.id, a.events.len());
            for e in &a.events {
                let params: Vec<String> = e.params.chunks(4).map(|c| hex(c)).collect();
                let ffx = e.as_ffx().map(|(id, d)| format!(" ffx={id} dummy={d}"));
                let snd = e
                    .as_sound()
                    .map(|(a, b)| format!(" sound={a}:{b}"))
                    .unwrap_or_default();
                let kind = if e.kind == KIND_FFX { "FFX" } else { "" };
                println!(
                    "    [{:>6}..{:>6}] kind {:>3} {kind} params[{}] {}{}",
                    e.start_frame,
                    e.end_frame,
                    e.kind,
                    params.join(" "),
                    ffx.unwrap_or_default(),
                    snd
                );
            }
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::new();
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
