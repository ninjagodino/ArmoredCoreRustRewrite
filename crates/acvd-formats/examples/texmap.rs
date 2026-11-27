//! Scratch: print an ASCII alpha map of a TPF texture region (first mip).
use acvd_formats::{tpf, vfs};

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let asset = &a[0];
    let name = a[1].to_lowercase();
    let (x0, y0, x1, y1): (usize, usize, usize, usize) =
        (a[2].parse()?, a[3].parse()?, a[4].parse()?, a[5].parse()?);
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    let d = vfs::open(&disc, asset)?;
    let t = tpf::read(&d)?;
    let tex: &tpf::Texture = t
        .textures
        .iter()
        .find(|t| t.name.to_lowercase() == name)
        .expect("texture");
    let rgba = match tex.format {
        0 => {
            let lin = tex.linear(t.platform, &d, 8)?;
            tpf::decode_bc1(&lin, tex.width, tex.height)?
        }
        5 | 33 => {
            let lin = tex.linear(t.platform, &d, 16)?;
            tpf::decode_bc3(&lin, tex.width, tex.height)?
        }
        f => anyhow::bail!("fmt {f} unsupported"),
    };
    println!(
        "{} {}x{} region {x0},{y0}..{x1},{y1}",
        tex.name, tex.width, tex.height
    );
    for y in y0..y1.min(tex.height as usize) {
        let mut row = String::new();
        for x in x0..x1.min(tex.width as usize) {
            let p = &rgba[(y * tex.width as usize + x) * 4..][..4];
            row.push(match p[3] {
                0 => ' ',
                1..=63 => '.',
                64..=127 => ':',
                128..=191 => '*',
                192..=223 => '%',
                224..=254 => '+',
                _ => '#',
            });
        }
        println!("{y:3} {row}");
    }
    Ok(())
}
