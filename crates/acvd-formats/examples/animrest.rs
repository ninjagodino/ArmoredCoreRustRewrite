//! Temporary: ANI rest vs first/last key translations per bone.
use acvd_formats::{ani, vfs};

fn main() -> anyhow::Result<()> {
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    for path in std::env::args().skip(1) {
        let clip = ani::read(&disc.asset(&path)?)?;
        println!("== {path} {} frames", clip.frames);
        for (i, b) in clip.bones.iter().enumerate() {
            let name = b.rest.as_ref().map_or("?", |r| r.name.as_str()).to_string();
            let rest = b.rest.as_ref().map_or([0.0; 3], |r| r.translation);
            let first = b
                .track
                .keys
                .first()
                .and_then(|k| k.translation)
                .map_or([0.0; 3], |t| t[0]);
            let last = b
                .track
                .keys
                .last()
                .and_then(|k| k.translation)
                .map_or([0.0; 3], |t| t[0]);
            println!(
                "{i:2} {name:14} rest=({:.1},{:.1},{:.1}) first=({:.1},{:.1},{:.1}) last=({:.1},{:.1},{:.1})",
                rest[0], rest[1], rest[2], first[0], first[1], first[2], last[0], last[1], last[2]
            );
        }
    }
    Ok(())
}
