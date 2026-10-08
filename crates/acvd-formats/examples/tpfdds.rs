//! Writes the textures of one TPF asset as DDS files for viewing:
//! `tpfdds <asset path> [texture name]` (`private/tmp/t2/dds/x360_<name>.dds`; with `ALL`
//! set, every level and face into `private/tmp/t2/ddsall/`). Tiled data is untiled first; codes
//! 23/24/25 are written raw (`.bin`).
use acvd_formats::{tpf, vfs};

fn dds(w: u32, h: u32, fourcc: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut o = b"DDS ".to_vec();
    let mut u = |v: u32| o.extend_from_slice(&v.to_le_bytes());
    u(124);
    u(0x1 | 0x2 | 0x4 | 0x1000 | 0x80000);
    u(h);
    u(w);
    u(body.len() as u32);
    u(0);
    u(1);
    for _ in 0..11 {
        u(0);
    }
    u(32);
    u(4);
    o.extend_from_slice(fourcc);
    for _ in 0..5 {
        o.extend_from_slice(&0u32.to_le_bytes());
    }
    o.extend_from_slice(&0x1000u32.to_le_bytes());
    for _ in 0..4 {
        o.extend_from_slice(&0u32.to_le_bytes());
    }
    o.extend_from_slice(body);
    o
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let asset = &args[0];
    let want = args.get(1).cloned();
    let root = vfs::repo_root();
    let out = root.join(if std::env::var_os("ALL").is_some() {
        "private/tmp/t2/ddsall"
    } else {
        "private/tmp/t2/dds"
    });
    std::fs::create_dir_all(&out)?;
    {
        let tag = "x360";
        let disc = vfs::Disc::open(&root.join(vfs::X360_ISO))?;
        let d = vfs::open(&disc, asset)?;
        let t = tpf::read(&d)?;
        for (i, tex) in t.textures.iter().enumerate() {
            if want
                .as_ref()
                .is_some_and(|w| !tex.name.eq_ignore_ascii_case(w))
            {
                continue;
            }
            let (bb, cc) = match tex.format {
                0 => (8, b"DXT1"),
                5 | 33 => (16, b"DXT5"),
                23 | 24 | 25 => {
                    let bb = if tex.format == 23 { 16 } else { 8 };
                    let lin = tex.linear(t.platform, &d, bb)?;
                    let n = (tex.width as usize).div_ceil(4)
                        * (tex.height as usize).div_ceil(4)
                        * bb as usize;
                    let file = out.join(format!(
                        "{tag}_{}_{}x{}_f{}.bin",
                        tex.name, tex.width, tex.height, tex.format
                    ));
                    std::fs::write(&file, &lin[..n])?;
                    println!("{tag} #{i} {} raw -> {}", tex.name, file.display());
                    continue;
                }
                f => {
                    println!(
                        "{tag} #{i} {} format {f} size {} {}x{} mips {}: {}",
                        tex.name,
                        tex.size,
                        tex.width,
                        tex.height,
                        tex.mipmaps,
                        hex(&tex.data(&d)?[..32.min(tex.size as usize)])
                    );
                    continue;
                }
            };
            let lin = tex.linear(t.platform, &d, bb)?;
            if std::env::var_os("ALL").is_some() {
                for face in 0..tex.faces() {
                    for level in 0..tex.levels() {
                        let (at, len) = tpf::block_level_span(
                            tex.width,
                            tex.height,
                            tex.levels(),
                            face,
                            level,
                            bb,
                        );
                        let (w, h) = (
                            (tex.width as u32 >> level).max(1),
                            (tex.height as u32 >> level).max(1),
                        );
                        let file = out.join(format!("{tag}_{}_f{face}_l{level}.dds", tex.name));
                        std::fs::write(
                            &file,
                            dds(w, h, cc, &lin[at as usize..(at + len) as usize]),
                        )?;
                    }
                }
                println!(
                    "{tag} #{i} {} {} levels x {} faces",
                    tex.name,
                    tex.levels(),
                    tex.faces()
                );
                continue;
            }
            let n =
                (tex.width as usize).div_ceil(4) * (tex.height as usize).div_ceil(4) * bb as usize;
            let file = out.join(format!(
                "{tag}_{}.dds",
                tex.name.replace(['/', '\\', ' '], "_")
            ));
            std::fs::write(
                &file,
                dds(tex.width as u32, tex.height as u32, cc, &lin[..n]),
            )?;
            println!(
                "{tag} #{i} {} fmt {} {}x{} -> {}",
                tex.name,
                tex.format,
                tex.width,
                tex.height,
                file.display()
            );
        }
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
