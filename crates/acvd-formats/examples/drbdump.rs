//! Prints a DRB layout: sections, textures, then every dialog with its objects (control class,
//! shape). Unknown shape classes are followed by their raw record bytes.
//! `drbdump [--disc PATH] LAYOUT [DIALOG...]` (`sortie` or a full `.drb.dcx` asset path).
use acvd_formats::drb::{self, Shape};
use acvd_formats::vfs;

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
    let name = args.first().cloned().unwrap_or_else(|| "sortie".into());
    let asset = if name.ends_with(".drb.dcx") { name } else { format!("lang/en/menu/{name}.drb.dcx") };
    let data = disc.asset(&asset)?;
    let d = drb::read(&data)?;
    let wanted = &args[1.min(args.len())..];
    if wanted.is_empty() {
        for s in &d.sections {
            println!("section {:5} count {:5} size {:#x} at {:#x}", s.tag, s.count, s.size, s.offset);
        }
        for (i, t) in d.textures.iter().enumerate() {
            println!("texture {i}: {} ({})", t.name, t.path);
        }
    }
    for (i, dlg) in d.dialogs.iter().enumerate() {
        if !wanted.is_empty() && !wanted.iter().any(|w| w == &dlg.name) {
            continue;
        }
        println!("dialog {i} `{}` {}x{} ({} objects)", dlg.name, dlg.size[0], dlg.size[1], dlg.objects.len());
        for o in &dlg.objects {
            println!("  {:24} {:16} {:?}", o.name, o.control, o.shape);
            if let Shape::Other { class, .. } = &o.shape {
                if let Some(raw) = raw_record(&data, &d, class, &o.name) {
                    println!("    raw {raw}");
                }
            }
        }
    }
    Ok(())
}

/// Hex of the first 64 bytes at the `RPHS` record of the first `PAHS` entry of class `class`
/// that `object` uses (found by scanning `OGLD` for the object name).
fn raw_record(data: &[u8], d: &drb::Drb, class: &str, object: &str) -> Option<String> {
    let body = |tag: &str| d.section(tag).map(|s| &data[s.offset..s.offset + s.size]);
    let be = |b: &[u8], at: usize| b.get(at..at + 4).map(|w| u32::from_be_bytes(w.try_into().unwrap()));
    let (rts, ogld, pahs, rphs) = (body("RTS")?, body("OGLD")?, body("PAHS")?, body("RPHS")?);
    let string = |at: u32| -> String {
        let units: Vec<u16> = rts[at as usize..].chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
        String::from_utf16_lossy(&units)
    };
    for o in (0..ogld.len()).step_by(0x20) {
        if string(be(ogld, o)?) != object {
            continue;
        }
        let shape = be(ogld, o + 4)? as usize;
        if string(be(pahs, shape)?) != class {
            continue;
        }
        let at = be(pahs, shape + 4)? as usize;
        let end = (at + 64).min(rphs.len());
        return Some(rphs[at..end].chunks(4).map(|w| w.iter().map(|b| format!("{b:02x}")).collect::<String>()).collect::<Vec<_>>().join(" "));
    }
    None
}
