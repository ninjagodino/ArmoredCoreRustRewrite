//! Scratch: verify the lock-cursor CursorLine gauge composite (sprite alpha AND mask alpha >= 1).
use acvd_formats::{tpf, vfs};

fn rgba(tex: &tpf::Texture, d: &[u8], platform: u8) -> anyhow::Result<Vec<u8>> {
    Ok(match tex.format {
        0 => tpf::decode_bc1(&tex.linear(platform, d, 8)?, tex.width, tex.height)?,
        5 | 33 => tpf::decode_bc3(&tex.linear(platform, d, 16)?, tex.width, tex.height)?,
        f => anyhow::bail!("fmt {f}"),
    })
}

fn main() -> anyhow::Result<()> {
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    let d = vfs::open(&disc, "lang/en/menu/sortie.tpf.dcx")?;
    let t = tpf::read(&d)?;
    let find = |n: &str| {
        let tex = t.textures.iter().find(|x| x.name == n).unwrap();
        (tex, rgba(tex, &d, t.platform).unwrap())
    };
    let (spr, spr_px) = find("ACV_FE_LockCursor_outline");
    let (mask, mask_px) = find("ACV_OW_overlay_02");
    let sample = |px: &[u8], tex: &tpf::Texture, u: f32, v: f32| -> [u8; 4] {
        let x = (u * tex.width as f32)
            .floor()
            .clamp(0.0, tex.width as f32 - 1.0) as usize;
        let y = (v * tex.height as f32)
            .floor()
            .clamp(0.0, tex.height as f32 - 1.0) as usize;
        let i = (y * tex.width as usize + x) * 4;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    };
    // (name, sprite rect x0,y0,x1,y1, uv x0,y0,x1,y1, flip_x, flip_y, inner corner x,y)
    let wings = [
        (
            "E", 14.0, 37.0, 128.0, 64.0, 7.0, 11.0, 121.0, 38.0, true, false, 128.0, 64.0,
        ),
        (
            "T", 0.0, 37.0, 104.0, 64.0, 7.0, 93.0, 111.0, 120.0, false, false, 0.0, 64.0,
        ),
        (
            "L", 24.0, 0.0, 128.0, 27.0, 7.0, 93.0, 111.0, 120.0, true, true, 128.0, 0.0,
        ),
        (
            "R", 0.0, 0.0, 104.0, 27.0, 7.0, 93.0, 111.0, 120.0, false, true, 0.0, 0.0,
        ),
    ];
    for (name, rx0, ry0, rx1, ry1, ux0, uy0, ux1, uy1, fx, fy, ix, iy) in wings {
        let (mut n, mut minx, mut miny, mut maxx, mut maxy) =
            (0u32, f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        let mut near = 0u32;
        for py in 0..(ry1 - ry0) as usize {
            for px in 0..(rx1 - rx0) as usize {
                let u = (px as f32 + 0.5) / (rx1 - rx0);
                let v = (py as f32 + 0.5) / (ry1 - ry0);
                let (u0, u1) = if fx { (ux1, ux0) } else { (ux0, ux1) };
                let (v0, v1) = if fy { (uy1, uy0) } else { (uy0, uy1) };
                let su = (u0 + (u1 - u0) * u) / spr.width as f32;
                let sv = (v0 + (v1 - v0) * v) / spr.height as f32;
                let s = sample(&spr_px, spr, su, sv);
                let m = sample(&mask_px, mask, su, sv);
                if s[3] > 0 && m[3] == 255 {
                    n += 1;
                    let x = rx0 + px as f32 + 0.5;
                    let y = ry0 + py as f32 + 0.5;
                    minx = minx.min(x);
                    miny = miny.min(y);
                    maxx = maxx.max(x);
                    maxy = maxy.max(y);
                    if (x - ix).abs() <= 34.0 && (y - iy).abs() <= 34.0 {
                        near += 1;
                    }
                }
            }
        }
        println!(
            "{name}: visible={n} near_inner={near} bbox=({minx:.0},{miny:.0})..({maxx:.0},{maxy:.0}) inner=({ix:.0},{iy:.0})"
        );
    }
    Ok(())
}
