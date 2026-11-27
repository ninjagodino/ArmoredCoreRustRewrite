//! Scratch: dump a CCM font's glyph codepoints and UV rects.
use acvd_formats::{ccm, fontdef, vfs};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let id: u32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(10);
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    let defs = fontdef::read(&disc.asset("font/fontdef.xml")?, disc.platform())?;
    let (folder, file) = defs.file(id).expect("font id");
    let data = disc.asset(&format!("font/{folder}/{file}"))?;
    let c = ccm::read(&data)?;
    println!(
        "font {id} {folder}/{file} line {} tex {}x{} sheets {}",
        c.line_height, c.tex_width, c.tex_height, c.texture_count
    );
    for (i, g) in c.glyphs.iter().enumerate() {
        let code = find_code(&c, i as i32);
        println!(
            "U+{code:04X} {:?}\tuv=({:.0},{:.0})-({:.0},{:.0}) tex{} {:.0}x{:.0} pre{} adv{}",
            char::from_u32(code as u32),
            g.uv0[0] * c.tex_width as f32,
            g.uv0[1] * c.tex_height as f32,
            g.uv1[0] * c.tex_width as f32,
            g.uv1[1] * c.tex_height as f32,
            g.texture,
            g.width,
            (g.uv1[1] - g.uv0[1]) * c.tex_height as f32,
            g.pre_space,
            g.advance
        );
    }
    Ok(())
}

fn find_code(c: &ccm::Ccm, glyph: i32) -> u16 {
    for group in &c.groups {
        let n = (group.last - group.first + 1) as i32;
        if glyph >= group.glyph && glyph < group.glyph + n {
            return (group.first as i32 + (glyph - group.glyph)) as u16;
        }
    }
    0xFFFF
}
