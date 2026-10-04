//! Reads files out of the owned disc through any nesting of DCX and BND3 containers.
//!
//! An asset path is a USRDIR-relative disc path followed by binder entry names, separated by
//! `|`: `model/ac/parts/arm/am0010/am0010_m.bnd.dcx|am0010.flv`. An entry may also be named by
//! its index as `#N`, for binders that repeat a name. DCX layers are expanded wherever they appear.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{bnd3, dcx};

pub const SEPARATOR: char = '|';

/// Expands `data` if it is a DCX container; returns it unchanged otherwise.
pub fn undcx(data: Vec<u8>) -> Result<Vec<u8>> {
    if dcx::is_dcx(&data) {
        dcx::decompress(&data)
    } else {
        Ok(data)
    }
}

/// The disc's `USRDIR`, given either the dump root or `USRDIR` itself.
pub fn usrdir(disc: &Path) -> PathBuf {
    for cand in [disc.join("PS3_GAME").join("USRDIR"), disc.join("USRDIR")] {
        if cand.is_dir() {
            return cand;
        }
    }
    disc.to_path_buf()
}

/// Bytes of one entry of a binder (by name, or `#N` for index N), with zlib and DCX layers removed.
pub fn entry(binder: &[u8], name: &str) -> Result<Vec<u8>> {
    let b = bnd3::read(binder)?;
    let found = match name.strip_prefix('#').and_then(|n| n.parse::<usize>().ok()) {
        Some(i) => b.entries.get(i),
        None => b.entries.iter().find(|e| e.name.as_deref() == Some(name)),
    };
    let e = found.with_context(|| format!("no entry `{name}`"))?;
    undcx(e.contents(binder)?.into_owned())
}

/// Opens an asset path (see module docs) under `usrdir`.
pub fn open(usrdir: &Path, asset: &str) -> Result<Vec<u8>> {
    let mut parts = asset.split(SEPARATOR);
    let file = parts.next().unwrap_or_default();
    let raw = std::fs::read(usrdir.join(file)).with_context(|| format!("reading {file}"))?;
    let mut data = undcx(raw).with_context(|| format!("expanding {file}"))?;
    for name in parts {
        data = entry(&data, name).with_context(|| format!("opening `{name}` in {asset}"))?;
    }
    Ok(data)
}
