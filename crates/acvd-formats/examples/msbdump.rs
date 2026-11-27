//! Temporary: list MSB sections, EVENT_PARAM_ST and POINT_PARAM_ST records.
use acvd_formats::reader::Be;

fn section(data: &[u8], name: &str) -> anyhow::Result<Vec<usize>> {
    let r = Be(data);
    let mut at = 0usize;
    for _ in 0..64 {
        let count = r.u32(at + 8)? as usize;
        let offs: Vec<usize> = (0..count)
            .map(|i| r.u32(at + 12 + 4 * i).map(|v| v as usize))
            .collect::<anyhow::Result<_>>()?;
        let sname = r.cstr_sjis(r.u32(at + 4)? as usize)?;
        println!("section {sname:24} at {at:#x} count={count}");
        if sname == name {
            return Ok(offs[..count - 1].to_vec());
        }
        match offs[count - 1] {
            0 => break,
            next => at = next,
        }
    }
    Ok(Vec::new())
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("msb path");
    let d = std::fs::read(path)?;
    let r = Be(&d);
    for off in section(&d, "EVENT_PARAM_ST")? {
        let name_rel = r.u32(off)? as usize;
        let name = r.cstr_sjis(off + name_rel).unwrap_or_default();
        println!(
            "EV off={off:#x} name={name:20} id={} kind={} index={}",
            r.i32(off + 4)?,
            r.u32(off + 8)?,
            r.u32(off + 0xc)?,
        );
    }
    for m in acvd_formats::msb::models(&d)? {
        println!(
            "MODEL kind={} sky={} light_set={} name={}",
            m.kind, m.is_sky, m.light_set, m.name
        );
    }
    for p in acvd_formats::msb::points(&d)? {
        println!(
            "POINT kind={} idx={} shape={} name={:20} t=({:.2},{:.2},{:.2}) rot=({:.2},{:.2},{:.2}) shape_data={:?}",
            p.kind,
            p.kind_index,
            p.shape,
            p.name,
            p.translation[0],
            p.translation[1],
            p.translation[2],
            p.rotation_deg[0],
            p.rotation_deg[1],
            p.rotation_deg[2],
            p.shape_data
        );
    }
    Ok(())
}
