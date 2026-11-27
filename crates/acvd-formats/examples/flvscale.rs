use acvd_formats::{flver, vfs};
fn main() -> anyhow::Result<()> {
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    for model in std::env::args().skip(1) {
        let asset = if model.contains('|') {
            model.clone()
        } else {
            format!("model/obj/{model}/{model}_m.bnd.dcx|{model}.flv")
        };
        let data = disc.asset(&asset)?;
        let f = flver::read(&data)?;
        println!("== {model}");
        for (i, b) in f.bones.iter().enumerate() {
            println!(
                "{i:2} parent={:2} {:<14} t=({:.2},{:.2},{:.2}) s=({:.4},{:.4},{:.4})",
                b.parent,
                b.name,
                b.translation[0],
                b.translation[1],
                b.translation[2],
                b.scale[0],
                b.scale[1],
                b.scale[2]
            );
        }
    }
    Ok(())
}
