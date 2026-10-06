//! Disc -> container sheets. Expands every DCX file and walks every BND3 binder (including
//! binders nested in binders), recording one row per entry with how it decodes and what it holds.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use acvd_formats::vfs::Disc;
use acvd_formats::{bnd3, dcx, flver, tpf};
use anyhow::{Context, Result};

use crate::extract::{dup_original, ext_of, magic_of};
use crate::model::*;

pub struct Summary {
    pub dcx: usize,
    pub binders: usize,
    pub entries: usize,
    pub textures: usize,
    pub models: usize,
    pub clips: usize,
}

#[derive(Default)]
struct Walk {
    groups: BTreeMap<String, Vec<BinderSheet>>,
    textures: BTreeMap<String, Vec<TpfSheet>>,
    models: BTreeMap<String, Vec<ModelSheet>>,
    motions: BTreeMap<String, Vec<MotionSet>>,
    exts: BTreeMap<String, (usize, u64, HashMap<String, usize>)>,
    entries: usize,
}

pub fn run(paths: &Paths, disc: &Disc, files: &[String]) -> Result<Summary> {
    let mut index = ArchiveIndex::default();
    let mut walk = Walk::default();
    for file in files {
        if acvd_formats::vfs::is_bundle(file) {
            continue;
        }
        let head = disc.head(file, 4)?;
        if !(dcx::is_dcx(&head) || bnd3::is_bnd3(&head) || tpf::is_tpf(&head) || head == b"FLVE"[..]) {
            continue;
        }
        let data = disc.read(file)?;
        if let Some(orig) = dup_original(file).filter(|o| disc.exists(o)) {
            if disc.read(&orig)? == data {
                continue;
            }
        }
        let file = file.clone();
        let data = if dcx::is_dcx(&data) {
            let read = dcx::read(&data).and_then(|d| Ok((d.decompress(&data)?, d)));
            match read {
                Ok((plain, d)) => {
                    index.dcx.push(DcxRow {
                        file: file.clone(),
                        uncompressed_size: d.uncompressed_size,
                        compressed_size: d.compressed_size,
                        chunks: d.chunks.len(),
                        trailing_bytes: d.trailing_bytes,
                        inner_magic: magic_of(&plain),
                        error: None,
                    });
                    plain
                }
                Err(e) => {
                    index.dcx.push(DcxRow {
                        file,
                        uncompressed_size: 0,
                        compressed_size: 0,
                        chunks: 0,
                        trailing_bytes: 0,
                        inner_magic: String::new(),
                        error: Some(format!("{e:#}")),
                    });
                    continue;
                }
            }
        } else {
            data
        };
        let group = file.split('/').next().unwrap_or_default().to_ascii_lowercase();
        if bnd3::is_bnd3(&data) {
            walk.binder(&group, &file, &data);
        } else if tpf::is_tpf(&data) {
            walk.texture_pack(&group, &file, &data);
        } else if flver::is_flver(&data) {
            walk.models.entry(group.clone()).or_default().push(crate::models::sheet(&file, &data));
        }
    }

    let mdir = paths.models();
    if mdir.is_dir() {
        std::fs::remove_dir_all(&mdir)?;
    }
    let mut models = 0;
    for (group, list) in walk.models {
        models += list.len();
        write_json(&mdir.join(format!("{group}.json")), &ModelGroup { group: group.clone(), models: list })?;
        index.model_groups.push(group);
    }

    let adir = paths.motions();
    if adir.is_dir() {
        std::fs::remove_dir_all(&adir)?;
    }
    let mut clips = 0;
    for (group, sets) in walk.motions {
        clips += sets.iter().map(|s| s.clips.len()).sum::<usize>();
        write_json(&adir.join(format!("{group}.json")), &MotionGroup { group: group.clone(), sets })?;
        index.motion_groups.push(group);
    }

    let dir = paths.archives();
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)?;
    }
    let binders = walk.groups.values().map(Vec::len).sum();
    for (group, list) in walk.groups {
        write_json(&dir.join(format!("{group}.json")), &ArchiveGroup { group: group.clone(), binders: list })?;
        index.groups.push(group);
    }
    index.entry_extensions = walk
        .exts
        .into_iter()
        .map(|(extension, (files, bytes, magics))| {
            let mut magics: Vec<_> = magics.into_iter().collect();
            magics.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            magics.truncate(6);
            ExtensionCount { extension, files, bytes, magics }
        })
        .collect();
    let tdir = paths.textures();
    if tdir.is_dir() {
        std::fs::remove_dir_all(&tdir)?;
    }
    let mut textures = 0;
    for (group, packs) in walk.textures {
        textures += packs.iter().map(|p| p.textures.len()).sum::<usize>();
        write_json(&tdir.join(format!("{group}.json")), &TextureGroup { group: group.clone(), packs })?;
        index.texture_groups.push(group);
    }
    let summary = Summary { dcx: index.dcx.len(), binders, entries: walk.entries, textures, models, clips };
    write_json(&paths.json().join("_archives.json"), &index)?;
    Ok(summary)
}

impl Walk {
    fn binder(&mut self, group: &str, path: &str, data: &[u8]) {
        let mut sheet = BinderSheet {
            path: path.to_owned(),
            version: String::new(),
            raw_format: 0,
            format: 0,
            big_endian: 0,
            bit_big_endian: 0,
            headers_end: 0,
            unexplained_byte: None,
            error: None,
            entries: Vec::new(),
        };
        let b = match bnd3::read(data) {
            Ok(b) => b,
            Err(e) => {
                sheet.error = Some(format!("{e:#}"));
                self.groups.entry(group.to_owned()).or_default().push(sheet);
                return;
            }
        };
        sheet.version = b.version.clone();
        sheet.raw_format = b.raw_format;
        sheet.format = b.format;
        sheet.big_endian = b.big_endian;
        sheet.bit_big_endian = b.bit_big_endian;
        sheet.headers_end = b.headers_end;
        match b.first_unexplained_byte(data) {
            Ok(at) => sheet.unexplained_byte = at,
            Err(e) => sheet.error = Some(format!("{e:#}")),
        }

        let mut nested = Vec::new();
        let mut motions = MotionSet { path: path.to_owned(), clips: Vec::new() };
        for (index, e) in b.entries.iter().enumerate() {
            let ext = e.name.as_deref().map(|n| ext_of(Path::new(n))).unwrap_or_else(|| "(none)".into());
            let mut row = EntryRow {
                index,
                id: e.id,
                name: e.name.clone(),
                flags: e.flags,
                encoding: if e.is_zlib() { "zlib" } else { "raw" }.into(),
                stored_size: e.stored_size,
                size: 0,
                dcx_trailing_bytes: 0,
                magic: String::new(),
                ext: ext.clone(),
                error: None,
            };
            let contents = e.contents(data).map(|c| c.into_owned()).and_then(|c| {
                if !dcx::is_dcx(&c) {
                    return Ok(c);
                }
                row.encoding = if e.is_zlib() { "zlib+dcx" } else { "dcx" }.into();
                let d = dcx::read(&c).context("inner DCX")?;
                row.dcx_trailing_bytes = d.trailing_bytes;
                d.decompress(&c).context("inner DCX")
            });
            match contents {
                Ok(c) => {
                    row.size = c.len();
                    row.magic = magic_of(&c);
                    let slot = self.exts.entry(ext.clone()).or_default();
                    slot.0 += 1;
                    slot.1 += c.len() as u64;
                    *slot.2.entry(row.magic.clone()).or_default() += 1;
                    if let Some(name) = &e.name {
                        let inner = format!("{path}{}{name}", acvd_formats::vfs::SEPARATOR);
                        if bnd3::is_bnd3(&c) {
                            nested.push((inner, c));
                        } else if tpf::is_tpf(&c) {
                            self.texture_pack(group, &inner, &c);
                        } else if flver::is_flver(&c) {
                            self.models.entry(group.to_owned()).or_default().push(crate::models::sheet(&inner, &c));
                        } else if ext == ".ani" {
                            motions.clips.push(crate::motions::clip(name, &c));
                        }
                    }
                }
                Err(err) => row.error = Some(format!("{err:#}")),
            }
            self.entries += 1;
            sheet.entries.push(row);
        }
        self.groups.entry(group.to_owned()).or_default().push(sheet);
        if !motions.clips.is_empty() {
            self.motions.entry(group.to_owned()).or_default().push(motions);
        }
        for (inner, c) in nested {
            self.binder(group, &inner, &c);
        }
    }

    fn texture_pack(&mut self, group: &str, path: &str, data: &[u8]) {
        let mut sheet = TpfSheet { path: path.to_owned(), platform: tpf::PLATFORM_PS3, flag2: 0, encoding: 0, error: None, textures: Vec::new() };
        match tpf::read(data) {
            Ok(t) => {
                sheet.platform = t.platform;
                sheet.flag2 = t.flag2;
                sheet.encoding = t.encoding;
                sheet.textures = t
                    .textures
                    .iter()
                    .enumerate()
                    .map(|(index, x)| TextureRow {
                        index,
                        name: x.name.clone(),
                        format: x.format,
                        kind: x.kind,
                        mipmaps: x.mipmaps,
                        levels: x.levels(),
                        faces: x.faces(),
                        width: x.width,
                        height: x.height,
                        size: x.size,
                        flags1: x.flags1,
                        unk1: x.unk1,
                        unk2: x.unk2,
                        floats: x.floats.clone(),
                        truncated: x.data(data).is_err(),
                    })
                    .collect();
            }
            Err(e) => sheet.error = Some(format!("{e:#}")),
        }
        self.textures.entry(group.to_owned()).or_default().push(sheet);
    }
}
