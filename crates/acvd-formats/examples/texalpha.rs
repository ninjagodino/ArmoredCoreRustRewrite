//! Temporary: alpha statistics for TPF entries (DXT1/DXT5), first mip.
use acvd_formats::{tpf, vfs};

fn main() -> anyhow::Result<()> {
    let asset = std::env::args().nth(1).expect("tpf asset");
    let wants: Vec<String> = std::env::args().skip(2).map(|s| s.to_lowercase()).collect();
    let root = vfs::repo_root();
    let disc = vfs::Disc::open(&root.join(vfs::X360_ISO))?;
    let d = vfs::open(&disc, &asset)?;
    let t = tpf::read(&d)?;
    for tex in t.textures.iter() {
        if !wants.is_empty() && !wants.iter().any(|w| tex.name.to_lowercase() == *w) {
            continue;
        }
        let rgba = match tex.format {
            0 => {
                let lin = tex.linear(t.platform, &d, 8)?;
                tpf::decode_bc1(&lin, tex.width, tex.height)?
            }
            5 | 33 => {
                let lin = tex.linear(t.platform, &d, 16)?;
                tpf::decode_bc3(&lin, tex.width, tex.height)?
            }
            f => {
                println!("{} fmt {f} unsupported", tex.name);
                continue;
            }
        };
        let n = (tex.width as usize) * (tex.height as usize);
        let (mut amin, mut amax, mut asum) = (255u8, 0u8, 0u64);
        let (mut rmax, mut gmax, mut bmax) = (0u8, 0u8, 0u8);
        let mut low_alpha_rgb = [0u64; 3];
        let mut low_n = 0u64;
        for i in 0..n {
            let p = &rgba[i * 4..i * 4 + 4];
            amin = amin.min(p[3]);
            amax = amax.max(p[3]);
            asum += p[3] as u64;
            rmax = rmax.max(p[0]);
            gmax = gmax.max(p[1]);
            bmax = bmax.max(p[2]);
            if p[3] < 128 {
                low_alpha_rgb[0] += p[0] as u64;
                low_alpha_rgb[1] += p[1] as u64;
                low_alpha_rgb[2] += p[2] as u64;
                low_n += 1;
            }
        }
        let mean = asum as f64 / n as f64;
        println!(
            "{} fmt{} {}x{} alpha min {amin} mean {mean:.1} max {amax}  rgbmax ({rmax},{gmax},{bmax})  lowA n {low_n} meanRGB ({:.0},{:.0},{:.0})",
            tex.name,
            tex.format,
            tex.width,
            tex.height,
            low_alpha_rgb[0] as f64 / low_n.max(1) as f64,
            low_alpha_rgb[1] as f64 / low_n.max(1) as f64,
            low_alpha_rgb[2] as f64 / low_n.max(1) as f64,
        );
        let _ = t;
    }
    Ok(())
}
