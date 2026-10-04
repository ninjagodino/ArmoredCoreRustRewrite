//! Prints the bones of a named `.ani` clip: `anibones <usrdir> <asset path>`.
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let anim = acvd_formats::ani::read(&acvd_formats::vfs::open(std::path::Path::new(&args[0]), &args[1])?)?;
    println!("{} frames, {} bones", anim.frames, anim.bones.len());
    for (i, b) in anim.bones.iter().enumerate() {
        let r = b.rest.as_ref();
        println!("{i:3} {:<16} parent {:?} keys {}", r.map_or("?", |r| r.name.as_str()), r.and_then(|r| r.parent), b.track.keys.len());
    }
    Ok(())
}
