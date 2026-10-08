//! XDVDFS: the Xbox 360 disc file system.
//!
//! The volume descriptor sits 32 sectors (0x800 bytes each) into the game partition: magic
//! `"MICROSOFT*XBOX*MEDIA"`, then little-endian `u32 root_sector, u32 root_size`. A directory is a
//! table of 4-byte-aligned nodes of a binary tree: `u16 left, u16 right` (child node offsets / 4,
//! 0 = none), `u32 start_sector, u32 size, u8 attributes, u8 name_len, name`; attribute bit 0x10
//! marks a subdirectory, whose table is at `start_sector`. A node of `FF FF FF FF` is padding.
//! Every file is one contiguous extent.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{bail, ensure, Context, Result};

pub const MAGIC: &[u8; 20] = b"MICROSOFT*XBOX*MEDIA";
pub const SECTOR: u64 = 0x800;
/// Game-partition starts tried in order: plain XDVDFS image, XGD3, XGD2 (ACVD), XGD1.
pub const PARTITIONS: [u64; 4] = [0, 0x2080000, 0xFD90000, 0x18300000];

const DIRECTORY: u8 = 0x10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    /// Byte offset in the image.
    pub offset: u64,
    pub size: u64,
}

pub struct Image {
    file: Mutex<File>,
    pub partition: u64,
    /// Lowercase `/`-separated paths, no leading slash.
    files: HashMap<String, Extent>,
}

impl Image {
    pub fn open(path: &Path) -> Result<Image> {
        let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut partition = None;
        for base in PARTITIONS {
            let mut head = [0u8; 28];
            file.seek(SeekFrom::Start(base + 32 * SECTOR))?;
            if file.read_exact(&mut head).is_ok() && &head[..20] == MAGIC {
                let le =
                    |at: usize| u32::from_le_bytes(head[at..at + 4].try_into().expect("4 bytes"));
                partition = Some((base, le(20), le(24)));
                break;
            }
        }
        let Some((base, root, root_size)) = partition else {
            bail!("{}: no XDVDFS volume", path.display())
        };
        let mut files = HashMap::new();
        let mut dirs = vec![(String::new(), root, root_size)];
        while let Some((prefix, sector, size)) = dirs.pop() {
            let mut table = vec![0u8; size as usize];
            file.seek(SeekFrom::Start(base + sector as u64 * SECTOR))?;
            file.read_exact(&mut table)
                .with_context(|| format!("directory `{prefix}`"))?;
            for (name, start, len, attr) in nodes(&table)? {
                let path = format!("{prefix}{}", name.to_ascii_lowercase());
                if attr & DIRECTORY != 0 {
                    if len != 0 {
                        dirs.push((path + "/", start, len));
                    }
                } else {
                    files.insert(
                        path,
                        Extent {
                            offset: base + start as u64 * SECTOR,
                            size: len as u64,
                        },
                    );
                }
            }
        }
        Ok(Image {
            file: Mutex::new(file),
            partition: base,
            files,
        })
    }

    /// Extent of a file (case-insensitive, `/` or `\` separators).
    pub fn find(&self, path: &str) -> Option<Extent> {
        self.files
            .get(
                &path
                    .replace('\\', "/")
                    .trim_start_matches('/')
                    .to_ascii_lowercase(),
            )
            .copied()
    }

    pub fn files(&self) -> impl Iterator<Item = (&str, Extent)> {
        self.files.iter().map(|(p, e)| (p.as_str(), *e))
    }

    /// `len` bytes at `offset` in the image.
    pub fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut out = vec![0u8; len];
        let mut f = self.file.lock().expect("image lock");
        f.seek(SeekFrom::Start(offset))?;
        f.read_exact(&mut out)
            .with_context(|| format!("reading {len:#x} bytes at {offset:#x}"))?;
        Ok(out)
    }

    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let e = self
            .find(path)
            .with_context(|| format!("no `{path}` on the disc"))?;
        self.read_at(e.offset, e.size as usize)
    }
}

/// Every node of one directory table: `(name, start_sector, size, attributes)`.
fn nodes(table: &[u8]) -> Result<Vec<(String, u32, u32, u8)>> {
    let mut out = Vec::new();
    let mut stack = vec![0usize];
    let mut seen = std::collections::HashSet::new();
    while let Some(off) = stack.pop() {
        if !seen.insert(off) || off + 14 > table.len() || table[off..off + 4] == [0xFF; 4] {
            continue;
        }
        let u16le = |at: usize| u16::from_le_bytes([table[at], table[at + 1]]);
        let u32le = |at: usize| u32::from_le_bytes(table[at..at + 4].try_into().expect("4 bytes"));
        let (left, right) = (u16le(off), u16le(off + 2));
        let (start, size, attr, len) = (
            u32le(off + 4),
            u32le(off + 8),
            table[off + 12],
            table[off + 13] as usize,
        );
        let name = table
            .get(off + 14..off + 14 + len)
            .context("directory name runs past its table")?;
        ensure!(!name.is_empty(), "empty name in directory node at {off:#x}");
        out.push((
            String::from_utf8_lossy(name).into_owned(),
            start,
            size,
            attr,
        ));
        for child in [left, right] {
            if child != 0 {
                stack.push(child as usize * 4);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(left: u16, right: u16, start: u32, size: u32, attr: u8, name: &str) -> Vec<u8> {
        let mut n = Vec::new();
        n.extend(left.to_le_bytes());
        n.extend(right.to_le_bytes());
        n.extend(start.to_le_bytes());
        n.extend(size.to_le_bytes());
        n.extend([attr, name.len() as u8]);
        n.extend(name.as_bytes());
        n.resize(n.len().next_multiple_of(4), 0xFF);
        n
    }

    #[test]
    fn walks_the_directory_tree() {
        let mut t = node(0, 5, 40, 0x800, DIRECTORY, "bind");
        assert_eq!(t.len(), 20);
        t.extend(node(0, 0, 41, 123, 0x80, "default.xex"));
        t.extend([0xFF; 8]);
        let mut got = nodes(&t).unwrap();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("bind".into(), 40, 0x800, DIRECTORY),
                ("default.xex".into(), 41, 123, 0x80)
            ]
        );
    }

    #[test]
    fn disc_image() {
        let iso = crate::vfs::repo_root().join(crate::vfs::X360_ISO);
        let Ok(img) = Image::open(&iso) else { return };
        assert_eq!(img.partition, 0xFD90000);
        assert_eq!(img.find("bind/dvdbnd5_layer0.bhd").unwrap().size, 231_136);
        assert_eq!(img.find("BIND\\Script.BHD").unwrap().size, 54_152);
        assert_eq!(&img.read("bind/script.bhd").unwrap()[..4], b"BHF3");
    }
}
