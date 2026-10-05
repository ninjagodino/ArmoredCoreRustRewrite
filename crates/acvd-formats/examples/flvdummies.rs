//! Prints the dummy points of a FLVER: `flvdummies <usrdir> <asset path>`.
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let f = acvd_formats::flver::read(&acvd_formats::vfs::open(std::path::Path::new(&args[0]), &args[1])?)?;
    let bone = |i: i16| usize::try_from(i).ok().and_then(|i| f.bones.get(i)).map_or("-", |b| b.name.as_str());
    for (i, d) in f.dummies.iter().enumerate() {
        println!(
            "{i:3} ref {:5} color {:?} parent {:<12} attach {:<12} pos {:?} fwd {:?} up {:?}",
            d.reference_id,
            d.color,
            bone(d.parent_bone),
            bone(d.attach_bone),
            d.position,
            d.forward,
            d.upward
        );
    }
    Ok(())
}
