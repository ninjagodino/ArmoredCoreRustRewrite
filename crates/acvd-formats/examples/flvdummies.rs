//! Prints the vertex bounds and dummy points of FLVERs: `flvdummies <disc> <asset path>...`.
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let disc = acvd_formats::vfs::Disc::open(std::path::Path::new(&args[0]))?;
    for asset in &args[1..] {
        let data = disc.asset(asset)?;
        let f = acvd_formats::flver::read(&data)?;
        let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
        for m in &f.meshes {
            for p in f.vertices(&data, m)?.positions {
                for k in 0..3 {
                    min[k] = min[k].min(p[k]);
                    max[k] = max[k].max(p[k]);
                }
            }
        }
        println!("{asset}: bounds {min:?} .. {max:?}");
        let bone = |i: i16| {
            usize::try_from(i)
                .ok()
                .and_then(|i| f.bones.get(i))
                .map_or("-", |b| b.name.as_str())
        };
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
    }
    Ok(())
}
