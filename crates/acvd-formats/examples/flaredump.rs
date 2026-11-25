//! Decodes `lensflare.tpf` sprites to BMP for inspection.
//! Usage: `flaredump [out dir]`
use acvd_formats::{tpf, vfs};

fn bmp(w: usize, h: usize, rgba: &[u8]) -> Vec<u8> {
    let row = (w * 3 + 3) & !3;
    let size = 54 + row * h;
    let mut o = Vec::with_capacity(size);
    o.extend_from_slice(b"BM");
    o.extend_from_slice(&(size as u32).to_le_bytes());
    o.extend_from_slice(&[0; 4]);
    o.extend_from_slice(&54u32.to_le_bytes());
    o.extend_from_slice(&40u32.to_le_bytes());
    o.extend_from_slice(&(w as i32).to_le_bytes());
    o.extend_from_slice(&(h as i32).to_le_bytes());
    o.extend_from_slice(&1u16.to_le_bytes());
    o.extend_from_slice(&24u16.to_le_bytes());
    o.extend_from_slice(&[0; 24]);
    for y in (0..h).rev() {
        let mut line = Vec::with_capacity(row);
        for x in 0..w {
            let p = (y * w + x) * 4;
            line.extend_from_slice(&[rgba[p + 2], rgba[p + 1], rgba[p]]);
        }
        line.resize(row, 0);
        o.extend_from_slice(&line);
    }
    o
}

fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "private/tmp/flare".to_string());
    std::fs::create_dir_all(&out)?;
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    let path = "model/filter/lensflare/lensflare_t.bnd|lensflare.tpf";
    let data = vfs::open(&disc, path)?;
    let hdr = tpf::read(&data)?;
    for tex in &hdr.textures {
        let lin = tex.linear(hdr.platform, &data, 8)?;
        let rgba = tpf::decode_bc1(&lin, tex.width, tex.height)?;
        let out_path = format!("{out}/{}.bmp", tex.name.to_ascii_lowercase());
        std::fs::write(&out_path, bmp(tex.width as usize, tex.height as usize, &rgba))?;
        println!("{} {}x{} -> {out_path}", tex.name, tex.width, tex.height);
    }
    Ok(())
}
