//! Dumps `ch_env/{map}_env.msb` dynamic point/spot lights (`POINT_PARAM_ST`) for a map,
//! or scans every `*_env.msb` on the disc for which maps carry spot lights.
//! Usage: `envlights m4000` / `envlights --scan`
use acvd_formats::{env, vfs};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;

    let dump = |path: &str| -> anyhow::Result<()> {
        let bytes = vfs::open(&disc, path)?;
        let env = env::read(&bytes)?;
        let points: Vec<_> = env.point_lights.iter().filter(|l| l.enabled).collect();
        let spots: Vec<_> = env.spot_lights.iter().filter(|l| l.enabled).collect();
        println!("{path}: {} point, {} spot", points.len(), spots.len());
        for p in points {
            println!(
                "  POINT {:?} pos=({:.2},{:.2},{:.2}) r={:.2} scale={:.2} diff={:?} falloff={:.2}",
                p.name,
                p.position[0],
                p.position[1],
                p.position[2],
                p.radius,
                p.scale,
                p.diffuse,
                p.falloff
            );
        }
        for s in spots {
            let d = s.direction();
            println!(
                "  SPOT {:?} pos=({:.2},{:.2},{:.2}) dir=({:.3},{:.3},{:.3}) far={:.2} fov_y={:.1} fov_x={:.1} scale={:.2} diff={:?} spec={:?} falloff={:.2}",
                s.name, s.position[0], s.position[1], s.position[2],
                d[0], d[1], d[2], s.far, s.fov_y_deg, s.fov_x_deg,
                s.scale, s.diffuse, s.specular, s.falloff
            );
        }
        Ok(())
    };

    if args.first().map(String::as_str) == Some("--scan") {
        for path in disc.list("model/map/ch_env") {
            if path.ends_with("_env.msb") {
                let _ = dump(&path);
            }
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--parts") {
        let map = args.get(1).map(String::as_str).unwrap_or("m4000");
        for file in [
            format!("model/map/{map}/{map}_map.msb"),
            format!("model/map/{map}/{map}_actest.msb"),
            format!("model/map/{map}/{map}_arena.msb"),
            format!("model/map/{map}/{map}.msb"),
        ] {
            let Ok(data) = vfs::open(&disc, &file) else {
                continue;
            };
            println!("== {file} ==");
            for p in acvd_formats::msb::parts(&data)? {
                println!(
                    "  {} model={} kind={} pos=({:.1},{:.1},{:.1}) rot={:?} ls={}",
                    p.name,
                    p.model,
                    p.kind,
                    p.translation[0],
                    p.translation[1],
                    p.translation[2],
                    p.rotation_deg,
                    p.light_set
                );
            }
            for p in acvd_formats::msb::points(&data)? {
                println!(
                    "  POINT {} kind={} idx={} shape={} pos=({:.1},{:.1},{:.1})",
                    p.name,
                    p.kind,
                    p.kind_index,
                    p.shape,
                    p.translation[0],
                    p.translation[1],
                    p.translation[2]
                );
            }
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--quads") {
        use std::collections::{BTreeMap, HashMap};
        let map = args.get(1).map(String::as_str).unwrap_or("m4000");
        let want = args.get(2).map(|s| s.to_ascii_lowercase());
        // Model placements (world transforms) from the map's MSBs, so a thin mesh can be
        // reported at every placed instance.
        let mut placements: HashMap<String, Vec<acvd_formats::msb::Part>> = HashMap::new();
        for file in [
            format!("model/map/{map}/{map}_map.msb"),
            format!("model/map/{map}/{map}_actest.msb"),
            format!("model/map/{map}/{map}_arena.msb"),
            format!("model/map/{map}/{map}.msb"),
        ] {
            let Ok(data) = vfs::open(&disc, &file) else {
                continue;
            };
            let Ok(parts) = acvd_formats::msb::parts(&data) else {
                continue;
            };
            for p in parts {
                placements.entry(p.model.clone()).or_default().push(p);
            }
        }
        let mut by_binder: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (model, parts) in &placements {
            if !want
                .as_ref()
                .is_none_or(|w| model.to_ascii_lowercase().starts_with(w))
            {
                continue;
            }
            let Some(part) = parts.first() else { continue };
            let binder = match part.kind {
                0 => Some(format!("model/map/{map}/{map}_m.dcx.bnd")),
                1 => Some(format!("model/obj/{0}/{0}_m.bnd.dcx", model)),
                _ => None,
            };
            if let Some(binder) = binder {
                by_binder.entry(binder).or_default().push(model.clone());
            }
        }
        for (binder, models) in by_binder {
            let Ok(data) = vfs::open(&disc, &binder) else {
                continue;
            };
            let Ok(b) = acvd_formats::bnd3::read(&data) else {
                continue;
            };
            for e in &b.entries {
                let Some(name) = e.name.as_deref() else {
                    continue;
                };
                let low = name.to_ascii_lowercase();
                if !low.ends_with(".flv") {
                    continue;
                }
                let stem = low.trim_end_matches(".flv").to_string();
                if !models.iter().any(|m| m.to_ascii_lowercase() == stem) {
                    continue;
                }
                let Ok(contents) = e.contents(&data).map(|c| c.into_owned()) else {
                    continue;
                };
                let Ok(raw) = vfs::undcx(contents) else {
                    continue;
                };
                let Ok(flv) = acvd_formats::flver::read(&raw) else {
                    continue;
                };
                let Some(parts) = placements.get(&stem) else {
                    continue;
                };
                for (mi, m) in flv.meshes.iter().enumerate() {
                    let Ok(verts) = flv.vertices(&raw, m) else {
                        continue;
                    };
                    let Some(&first) = verts.positions.first() else {
                        continue;
                    };
                    let (mut min, mut max) = (first, first);
                    for p in &verts.positions {
                        for a in 0..3 {
                            min[a] = min[a].min(p[a]);
                            max[a] = max[a].max(p[a]);
                        }
                    }
                    let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
                    let mut dims = size;
                    dims.sort_by(|a, b| a.total_cmp(b));
                    // Thin, large mesh = billboard / plane / shaft / decal candidate.
                    if std::env::var("QUADS_ALL").is_err() && (dims[0] > 2.0 || dims[2] < 4.0) {
                        continue;
                    }
                    let mat = flv.materials.get(m.material.max(0) as usize);
                    let (mname, mtd, tex) = mat.map_or(("?", "?", String::new()), |mat| {
                        let start = mat.texture_index.max(0) as usize;
                        let tex = flv
                            .textures
                            .iter()
                            .skip(start)
                            .take(mat.texture_count.max(0) as usize)
                            .map(|t| format!("{}={}", t.kind, t.path))
                            .collect::<Vec<_>>()
                            .join(",");
                        (mat.name.as_str(), mat.mtd.as_str(), tex)
                    });
                    let c = [
                        (min[0] + max[0]) * 0.5,
                        (min[1] + max[1]) * 0.5,
                        (min[2] + max[2]) * 0.5,
                    ];
                    for part in parts {
                        let xf = acvd_formats::flver::Xform::local(
                            part.translation,
                            part.rotation_deg.map(f32::to_radians),
                            part.scale,
                        );
                        let w = xf.apply(c);
                        println!(
                            "{stem:14} part={:14} mesh={mi:2} mat={mname:16} mtd={mtd:22} size=({:.1},{:.1},{:.1}) local=({:.1},{:.1},{:.1}) world=({:.1},{:.1},{:.1}) {tex}",
                            part.name, size[0], size[1], size[2], c[0], c[1], c[2],
                            w[0], w[1], w[2]
                        );
                    }
                }
            }
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--tex") {
        let map = args.get(1).map(String::as_str).unwrap_or("m4000");
        let want = args.get(2).map(|s| s.to_ascii_lowercase());
        let binder = format!("model/map/{map}/{map}_m.dcx.bnd");
        let data = vfs::open(&disc, &binder)?;
        let b = acvd_formats::bnd3::read(&data)?;
        for e in &b.entries {
            let Some(name) = e.name.as_deref() else {
                continue;
            };
            let low = name.to_ascii_lowercase();
            if !low.ends_with(".flv") || !want.as_ref().is_none_or(|w| low.starts_with(w)) {
                continue;
            }
            let Ok(contents) = e.contents(&data).map(|c| c.into_owned()) else {
                continue;
            };
            let Ok(raw) = vfs::undcx(contents) else {
                continue;
            };
            let Ok(flv) = acvd_formats::flver::read(&raw) else {
                continue;
            };
            println!("== {name} ==");
            for m in &flv.materials {
                println!(
                    "  mat {:24} {:20} flags={:#x} tex={}",
                    m.name, m.mtd, m.flags, m.texture_count
                );
                let start = m.texture_index.max(0) as usize;
                for t in flv
                    .textures
                    .iter()
                    .skip(start)
                    .take(m.texture_count.max(0) as usize)
                {
                    println!("      {} = {}", t.kind, t.path);
                }
            }
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--mtds") {
        use std::collections::BTreeMap;
        let map = args.get(1).map(String::as_str).unwrap_or("m4000");
        let binder = format!("model/map/{map}/{map}_m.dcx.bnd");
        let data = vfs::open(&disc, &binder)?;
        let b = acvd_formats::bnd3::read(&data)?;
        let mut by_mtd: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for e in &b.entries {
            let Some(name) = e.name.as_deref() else {
                continue;
            };
            if !name.to_ascii_lowercase().ends_with(".flv") {
                continue;
            }
            let Ok(contents) = e.contents(&data).map(|c| c.into_owned()) else {
                continue;
            };
            let Ok(raw) = vfs::undcx(contents) else {
                continue;
            };
            let Ok(flv) = acvd_formats::flver::read(&raw) else {
                continue;
            };
            for m in &flv.materials {
                by_mtd
                    .entry(m.mtd.clone())
                    .or_default()
                    .push((name.to_string(), m.name.clone()));
            }
        }
        for (mtd, uses) in &by_mtd {
            println!("{mtd}  ({})", uses.len());
        }
        for want in ["Map_Diffuse_Add", "Map_Diffuse _Grow", "Map_Speculer_Env"] {
            if let Some(uses) = by_mtd.get(want) {
                println!("== {want} ==");
                for (flv, mat) in uses {
                    println!("   {flv}  mat={mat}");
                }
            }
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--shafts-scan") {
        use std::collections::{BTreeMap, HashMap};
        let mut maps: Vec<String> = disc
            .list("model/map/ch_env")
            .into_iter()
            .filter_map(|p| {
                let f = p.rsplit('/').next()?;
                f.strip_suffix("_env.msb").map(str::to_string)
            })
            .collect();
        maps.sort();
        maps.dedup();
        for map in &maps {
            let mut placements: HashMap<String, Vec<acvd_formats::msb::Part>> = HashMap::new();
            for file in [
                format!("model/map/{map}/{map}_map.msb"),
                format!("model/map/{map}/{map}_actest.msb"),
                format!("model/map/{map}/{map}_arena.msb"),
                format!("model/map/{map}/{map}.msb"),
            ] {
                let Ok(data) = vfs::open(&disc, &file) else {
                    continue;
                };
                let Ok(parts) = acvd_formats::msb::parts(&data) else {
                    continue;
                };
                for p in parts {
                    placements.entry(p.model.clone()).or_default().push(p);
                }
            }
            let mut by_binder: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (model, parts) in &placements {
                let Some(part) = parts.first() else { continue };
                let binder = match part.kind {
                    0 => Some(format!("model/map/{map}/{map}_m.dcx.bnd")),
                    1 => Some(format!("model/obj/{0}/{0}_m.bnd.dcx", model)),
                    _ => None,
                };
                if let Some(binder) = binder {
                    by_binder.entry(binder).or_default().push(model.clone());
                }
            }
            for (binder, models) in by_binder {
                let Ok(data) = vfs::open(&disc, &binder) else {
                    continue;
                };
                let Ok(b) = acvd_formats::bnd3::read(&data) else {
                    continue;
                };
                for e in &b.entries {
                    let Some(name) = e.name.as_deref() else {
                        continue;
                    };
                    let low = name.to_ascii_lowercase();
                    if !low.ends_with(".flv") {
                        continue;
                    }
                    let stem = low.trim_end_matches(".flv").to_string();
                    if !models.iter().any(|m| m.to_ascii_lowercase() == stem) {
                        continue;
                    }
                    let Ok(contents) = e.contents(&data).map(|c| c.into_owned()) else {
                        continue;
                    };
                    let Ok(raw) = vfs::undcx(contents) else {
                        continue;
                    };
                    let Ok(flv) = acvd_formats::flver::read(&raw) else {
                        continue;
                    };
                    let Some(parts) = placements.get(&stem) else {
                        continue;
                    };
                    for (mi, m) in flv.meshes.iter().enumerate() {
                        let Some(mat) = flv.materials.get(m.material.max(0) as usize) else {
                            continue;
                        };
                        let mtd_low = mat.mtd.to_ascii_lowercase();
                        if !mtd_low.contains("_add") {
                            continue;
                        }
                        let start = mat.texture_index.max(0) as usize;
                        let tex = flv
                            .textures
                            .iter()
                            .skip(start)
                            .take(mat.texture_count.max(0) as usize)
                            .find(|t| t.kind == "g_DiffuseTexture")
                            .map(|t| t.path.to_ascii_lowercase())
                            .unwrap_or_default();
                        let lighty = tex.contains("light")
                            || tex.contains("shaft")
                            || tex.contains("beam")
                            || tex.contains("hika")
                            || tex.contains("flare")
                            || tex.contains("glow")
                            || mtd_low.contains("ene_")
                            || mtd_low.contains("nolight");
                        if !lighty {
                            continue;
                        }
                        let Ok(verts) = flv.vertices(&raw, m) else {
                            continue;
                        };
                        let Some(&first) = verts.positions.first() else {
                            continue;
                        };
                        let (mut min, mut max) = (first, first);
                        for p in &verts.positions {
                            for a in 0..3 {
                                min[a] = min[a].min(p[a]);
                                max[a] = max[a].max(p[a]);
                            }
                        }
                        let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
                        let c = [
                            (min[0] + max[0]) * 0.5,
                            (min[1] + max[1]) * 0.5,
                            (min[2] + max[2]) * 0.5,
                        ];
                        for part in parts {
                            let xf = acvd_formats::flver::Xform::local(
                                part.translation,
                                part.rotation_deg.map(f32::to_radians),
                                part.scale,
                            );
                            let w = xf.apply(c);
                            println!(
                                "{map:6} {stem:14} part={:14} mesh={mi:2} mtd={mtd_low:20} tex={tex:14} verts={:5} size=({:.1},{:.1},{:.1}) world=({:.1},{:.1},{:.1})",
                                part.name,
                                verts.positions.len(),
                                size[0],
                                size[1],
                                size[2],
                                w[0],
                                w[1],
                                w[2]
                            );
                        }
                    }
                }
            }
        }
        return Ok(());
    }
    let map = args.first().map(String::as_str).unwrap_or("m4000");
    dump(&format!("model/map/ch_env/{map}_env.msb"))?;
    Ok(())
}
