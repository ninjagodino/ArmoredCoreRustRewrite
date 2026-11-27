//! Scratch: print a bone's translation keys from an ANI clip.
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("ani path");
    let bone = std::env::args().nth(2).unwrap_or_else(|| "body".into());
    let d = std::fs::read(&path)?;
    let a = acvd_formats::ani::read(&d)?;
    println!("{} frames {} bones", a.frames, a.bones.len());
    for b in &a.bones {
        let Some(r) = &b.rest else { continue };
        if r.name != bone {
            continue;
        }
        println!(
            "bone {} keys {} rest {:?}",
            r.name,
            b.track.keys.len(),
            r.translation
        );
        for k in b.track.keys.iter().take(20) {
            println!("  {:?}", k);
        }
    }
    Ok(())
}
