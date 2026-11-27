//! Scratch: scan menu DRBs for objects whose names mention buttons/pads/keys.
use acvd_formats::{drb, vfs};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let prefix = args
        .first()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "lang/en/menu".into());
    let filter = args.get(1).map(|s| s.to_ascii_lowercase());
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    for file in disc.files() {
        let low = file.to_ascii_lowercase();
        if !low.starts_with(&prefix) || !low.ends_with(".drb.dcx") {
            continue;
        }
        let Ok(data) = disc.asset(&file) else {
            continue;
        };
        let Ok(d) = drb::read(&data) else {
            eprintln!("{file}: not drb");
            continue;
        };
        for tex in &d.textures {
            println!("{file}\tTEX\t{}\t{}", tex.name, tex.path);
        }
        for dlg in &d.dialogs {
            for o in &dlg.objects {
                if filter.as_deref() == Some("short") {
                    if let drb::Shape::Text {
                        source: drb::TextSource::Static(s),
                        font,
                        rect,
                        ..
                    } = &o.shape
                    {
                        if s.chars().count() <= 2 {
                            println!(
                                "{file}\tSHORT\t{}\t{}\t{:?}\tfont={font}\trect={rect:?}",
                                dlg.name, o.name, s
                            );
                        }
                    }
                    continue;
                }
                let hay = format!("{} {} {}", dlg.name, o.name, o.control).to_ascii_lowercase();
                if let Some(f) = &filter {
                    if !hay.contains(f.as_str()) {
                        continue;
                    }
                } else if !["button", "btn", "pad", "key", "guide"]
                    .iter()
                    .any(|k| hay.contains(k))
                {
                    continue;
                }
                let extra = match &o.shape {
                    drb::Shape::Sprite {
                        rect, uv, texture, ..
                    } => {
                        format!("sprite rect={rect:?} uv={uv:?} tex={texture:?}")
                    }
                    drb::Shape::Dialog { rect, dialog, .. } => {
                        format!("dialog rect={rect:?} -> {dialog:?}")
                    }
                    drb::Shape::Text {
                        rect, source, font, ..
                    } => {
                        format!("text rect={rect:?} font={font} {source:?}")
                    }
                    s => format!("{s:?}"),
                };
                println!(
                    "{file}\tOBJ\t{}\tsize={:?}\t{}\t{}\t{extra}",
                    dlg.name, dlg.size, o.name, o.control
                );
            }
        }
    }
    Ok(())
}
