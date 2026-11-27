//! Scratch: print an ANI bone's translation keys nearest requested frames.
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("ani path");
    let bone = std::env::args().nth(2).unwrap_or_else(|| "body".into());
    let step: u32 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    let d = std::fs::read(&path)?;
    let a = acvd_formats::ani::read(&d)?;
    let b = a
        .bones
        .iter()
        .find(|b| b.rest.as_ref().is_some_and(|r| r.name == bone))
        .expect("bone");
    let keys = &b.track.keys;
    println!(
        "{} frames {} bone {} keys {}",
        path,
        a.frames,
        bone,
        keys.len()
    );
    let mut target = 0u32;
    while target <= a.frames {
        if let Some(k) = keys
            .iter()
            .min_by_key(|k| (k.frame as i64 - target as i64).abs())
        {
            let t = k.translation.map(|v| v[0]).unwrap_or_default();
            println!(
                "frame {:5} key {:5} t=({:.3},{:.3},{:.3})",
                target, k.frame, t[0], t[1], t[2]
            );
        }
        target += step;
    }
    Ok(())
}
