//! Reads files out of the owned 360 disc image through any nesting of DCX and BND3 containers.
//!
//! An asset path is a disc path followed by binder entry names, separated by `|`:
//! `model/ac/parts/arm/am0010/am0010_m.bnd.dcx|am0010.flv`. An entry may also be named by its
//! index as `#N`, for binders that repeat a name. DCX layers are expanded wherever they appear.
//!
//! A [`Disc`] is the 360 ISO. A path is looked up as a loose XDVDFS file (`movie/jp/*.wmv`),
//! then by [`bhd5::path_hash`] in
//! `bind/dvdbnd5_layer{0,1}.bhd` (data in `bind/dvdbnd_layer{0,1}.bdt`), then under `script/` in
//! the BHF3 `bind/script.bhd` / `.bdt`, then as a member of a load bundle ([`BUNDLES`]: BND3
//! binders whose member names are disc paths, e.g. `bind/boot.bnd|system\paramlist.xml`; the
//! first bundle holding a path wins, and no path differs between bundles). The BHD5 archives
//! store no names; listings come from `private/x360/dvdbnd_names.csv` (`archive,hash,size,path`).
//! An entry whose real path is unknown is named `_unknown/<dir>/<decimal hash>.<ext>` (the
//! name the PS3 disc's extracted files used, kept so sheet ids stay put) and is read by that hash.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::{bhd5, bnd3, dcx, xdvdfs};

pub const SEPARATOR: char = '|';

/// The 360 disc image, relative to the repo root.
pub const X360_ISO: &str = "armoredcoredumps/Armored Core - Verdict Day (USA)/Armored Core - Verdict Day (USA).iso";
/// Names of the BHD5 entries, relative to the repo root.
pub const X360_NAMES: &str = "private/x360/dvdbnd_names.csv";

const LAYERS: [(&str, &str); 2] = [
    ("bind/dvdbnd5_layer0.bhd", "bind/dvdbnd_layer0.bdt"),
    ("bind/dvdbnd5_layer1.bhd", "bind/dvdbnd_layer1.bdt"),
];
const SCRIPT: (&str, &str) = ("bind/script.bhd", "bind/script.bdt");
const SCRIPT_DIR: &str = "script/";
/// Load bundles, in lookup order: the boot binders (360 `$(Data)\bind\boot.bnd`,
/// `boot_2nd.bnd`), then every named `bind/mission/*.bnd` (`$(Data)\bind\mission\ch%04d.bnd`).
pub const BUNDLES: [&str; 3] = ["bind/boot.bnd", "bind/boot_2nd.bnd", "bind/mission/"];

/// Whether `file` is one of the [`BUNDLES`] (each member is also listed as its own disc file).
pub fn is_bundle(file: &str) -> bool {
    let k = key(file);
    BUNDLES.iter().any(|b| if b.ends_with('/') { k.starts_with(b) && k.ends_with(".bnd") && !k[b.len()..].contains('/') } else { k == *b })
}

/// The repo checkout this crate was built from.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The disc used when no `--disc` is given: the 360 ISO under `root`.
pub fn default_disc(root: &Path) -> PathBuf {
    root.join(X360_ISO)
}

/// Expands `data` if it is a DCX container; returns it unchanged otherwise.
pub fn undcx(data: Vec<u8>) -> Result<Vec<u8>> {
    if dcx::is_dcx(&data) {
        dcx::decompress(&data)
    } else {
        Ok(data)
    }
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

/// Opens an asset path (see module docs) on `disc`.
pub fn open(disc: &Disc, asset: &str) -> Result<Vec<u8>> {
    disc.asset(asset)
}

/// The 360 ISO, opened. Cheap to clone.
#[derive(Clone)]
pub struct Disc(Arc<X360>);

struct X360 {
    path: PathBuf,
    image: xdvdfs::Image,
    /// Per layer: `.bdt` offset in the image, entries by path hash.
    layers: Vec<(u64, HashMap<u32, bhd5::Entry>)>,
    /// `script.bdt` offset in the image, and `script.bhd` entries by lowercase disc path.
    script: (u64, HashMap<String, bnd3::Entry>),
    /// Load-bundle members by lowercase disc path: the bundle's offset in the image and the entry.
    bundled: HashMap<String, (u64, bnd3::Entry)>,
    /// Every named file: lowercase disc path to the path as named.
    names: BTreeMap<String, String>,
}

/// Lowercase, `/`-separated, no leading slash.
fn key(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_ascii_lowercase()
}

impl Disc {
    /// Opens a 360 ISO; entry names come from [`X360_NAMES`] in [`repo_root`].
    pub fn open(path: &Path) -> Result<Disc> {
        Self::open_with_names(path, &repo_root().join(X360_NAMES))
    }

    pub fn open_with_names(path: &Path, names: &Path) -> Result<Disc> {
        if !path.is_file() {
            bail!("no disc image at {}", path.display());
        }
        Ok(Disc(Arc::new(X360::open(path, names)?)))
    }

    /// The default disc under the repo root (see [`default_disc`]).
    pub fn default_for(root: &Path) -> Result<Disc> {
        Self::open(&default_disc(root))
    }

    /// The game's `$(Platform)` path variable (`font/s1_$(Platform)/` ...): `xbox`
    /// (`0x82303150` loads `$(Platform)` at `0x82001ad4` and `xbox` at `0x82001ae0`).
    pub fn platform(&self) -> &'static str {
        "xbox"
    }

    /// The image this disc reads.
    pub fn path(&self) -> &Path {
        &self.0.path
    }

    /// Raw bytes of one disc file (no DCX expansion).
    pub fn read(&self, file: &str) -> Result<Vec<u8>> {
        self.0.read(file)
    }

    pub fn exists(&self, file: &str) -> bool {
        self.0.exists(file)
    }

    /// Size of one disc file as stored (a DCX file's compressed size).
    pub fn size(&self, file: &str) -> Result<u64> {
        match self.0.locate(file) {
            Some(Loc::Image(_, size)) => Ok(size),
            Some(Loc::Packed(_, e)) => Ok(e.size.unwrap_or(e.stored_size) as u64),
            None => bail!("no `{file}` on the 360 disc"),
        }
    }

    /// The first `n` bytes of one disc file (all of it when shorter).
    pub fn head(&self, file: &str, n: usize) -> Result<Vec<u8>> {
        match self.0.locate(file) {
            Some(Loc::Image(at, size)) => self.0.image.read_at(at, n.min(size as usize)),
            _ => Ok(self.0.read(file)?.into_iter().take(n).collect()),
        }
    }

    /// Opens an asset path (see module docs).
    pub fn asset(&self, asset: &str) -> Result<Vec<u8>> {
        let mut parts = asset.split(SEPARATOR);
        let file = parts.next().unwrap_or_default();
        let mut data = undcx(self.read(file)?).with_context(|| format!("expanding {file}"))?;
        for name in parts {
            data = entry(&data, name).with_context(|| format!("opening `{name}` in {asset}"))?;
        }
        Ok(data)
    }

    /// Every named file path on the disc (disc-relative, `/`-separated), sorted.
    pub fn files(&self) -> Vec<String> {
        self.0.names.values().cloned().collect()
    }

    /// Paths of the named files directly in `dir` (disc-relative, `/`-separated), sorted.
    pub fn list(&self, dir: &str) -> Vec<String> {
        let prefix = format!("{}/", key(dir).trim_end_matches('/'));
        self.0.names.range(prefix.clone()..).take_while(|(k, _)| k.starts_with(&prefix)).filter(|(k, _)| !k[prefix.len()..].contains('/')).map(|(_, v)| v.clone()).collect()
    }
}

impl X360 {
    fn open(path: &Path, names_csv: &Path) -> Result<X360> {
        let image = xdvdfs::Image::open(path)?;
        let extent = |file: &str| image.find(file).with_context(|| format!("{}: no {file}", path.display()));
        let mut layers = Vec::new();
        for (bhd, bdt) in LAYERS {
            let entries = bhd5::read(&image.read(bhd)?).with_context(|| bhd.to_string())?;
            layers.push((extent(bdt)?.offset, entries.into_iter().map(|e| (e.hash, e)).collect()));
        }
        let script_index = bnd3::read_bhf3(&image.read(SCRIPT.0)?).context(SCRIPT.0)?;
        let mut names = BTreeMap::new();
        let mut script = HashMap::new();
        for e in script_index.entries {
            let Some(name) = &e.name else { continue };
            let path = format!("{SCRIPT_DIR}{}", name.replace('\\', "/"));
            names.insert(key(&path), path.clone());
            script.insert(key(&path), e);
        }
        for (file, _) in image.files() {
            names.insert(file.to_string(), file.to_string());
        }
        if let Ok(csv) = std::fs::read_to_string(names_csv) {
            for line in csv.lines().skip(1) {
                if let Some(p) = line.splitn(4, ',').nth(3).map(|p| p.trim_start_matches('/')).filter(|p| !p.is_empty()) {
                    names.insert(key(p), p.to_string());
                }
            }
        }
        let script = (extent(SCRIPT.1)?.offset, script);
        let mut x = X360 { path: path.to_path_buf(), script, image, layers, bundled: HashMap::new(), names };
        x.index_bundles()?;
        Ok(x)
    }

    /// Indexes the members of every [`BUNDLES`] binder (headers only) that no earlier source has.
    fn index_bundles(&mut self) -> Result<()> {
        let mut bundles = Vec::new();
        for b in BUNDLES {
            if b.ends_with('/') {
                bundles.extend(self.names.range(b.to_string()..).take_while(|(k, _)| k.starts_with(b)).filter(|(k, _)| k.ends_with(".bnd")).map(|(k, _)| k.clone()));
            } else {
                bundles.push(b.to_string());
            }
        }
        let mut bundled = HashMap::new();
        for b in bundles {
            let Some((at, size)) = self.archived(&b) else { continue };
            let head = self.image.read_at(at, 0x20.min(size as usize))?;
            let headers_end = u32::from_be_bytes(head.get(0x14..0x18).with_context(|| format!("{b}: short header"))?.try_into().expect("4 bytes"));
            let header = self.image.read_at(at, (headers_end as usize).min(size as usize))?;
            for e in bnd3::read_header(&header).with_context(|| b.clone())?.entries {
                let Some(name) = &e.name else { continue };
                let path = name.replace('\\', "/");
                let k = key(&path);
                if bundled.contains_key(&k) || self.image.find(&k).is_some() || self.archived(&k).is_some() || self.script.1.contains_key(&k) {
                    continue;
                }
                self.names.entry(k.clone()).or_insert(path);
                bundled.insert(k, (at, e));
            }
        }
        self.bundled = bundled;
        Ok(())
    }

    fn archived(&self, file: &str) -> Option<(u64, u32)> {
        let h = unknown_hash(file).unwrap_or_else(|| bhd5::path_hash(file));
        self.layers.iter().find_map(|(base, entries)| entries.get(&h).map(|e| (base + e.offset, e.size)))
    }

    fn locate(&self, file: &str) -> Option<Loc<'_>> {
        if let Some(e) = self.image.find(file) {
            return Some(Loc::Image(e.offset, e.size));
        }
        if let Some((at, size)) = self.archived(file) {
            return Some(Loc::Image(at, size as u64));
        }
        let k = key(file);
        if let Some(e) = self.script.1.get(&k) {
            return Some(Loc::Packed(self.script.0, e));
        }
        self.bundled.get(&k).map(|(at, e)| Loc::Packed(*at, e))
    }

    fn exists(&self, file: &str) -> bool {
        self.locate(file).is_some()
    }

    fn read(&self, file: &str) -> Result<Vec<u8>> {
        match self.locate(file) {
            Some(Loc::Image(at, size)) => self.image.read_at(at, size as usize).with_context(|| format!("reading {file}")),
            Some(Loc::Packed(base, e)) => {
                let stored = self.image.read_at(base + e.offset as u64, e.stored_size as usize)?;
                let at_zero = bnd3::Entry { offset: 0, ..e.clone() };
                Ok(at_zero.contents(&stored).with_context(|| format!("reading {file}"))?.into_owned())
            }
            None => bail!("no `{file}` on the 360 disc"),
        }
    }
}

/// Where a 360 file's bytes are: an image extent (loose or BHD5), or an entry of `script.bdt`
/// or of a load bundle, with that container's offset in the image.
enum Loc<'a> {
    Image(u64, u64),
    Packed(u64, &'a bnd3::Entry),
}

/// The repo's 360 ISO, for tests that skip when it is absent.
#[cfg(test)]
pub(crate) fn test_disc() -> Option<Disc> {
    let path = repo_root().join(X360_ISO);
    path.is_file().then(|| Disc::open(&path).unwrap())
}

/// The hash in an `_unknown/<dir>/<decimal hash>.<ext>` name.
fn unknown_hash(file: &str) -> Option<u32> {
    let k = key(file);
    let name = k.strip_prefix("_unknown/")?.rsplit('/').next()?;
    name.split('.').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::test_disc as iso;

    #[test]
    fn x360_disc_reads_every_source() {
        let Some(disc) = iso() else { return };
        assert_eq!(disc.read("param/accolor/color5001.bin").unwrap().len(), 856);
        assert_eq!(disc.read("/PARAM/coloringset.bin").unwrap().len(), 2121);
        assert_eq!(disc.read("movie/jp/tu_boost.wmv").unwrap().len(), 2_505_347);
        assert_eq!(&disc.read("script/acctrlparamcalc.lc").unwrap()[..5], b"\x1bLuaP");
        assert_eq!(disc.read("script/action/enemy/e7010.lc").unwrap().len(), 5538);
        assert!(disc.exists("model/ac/parts/hand/hl3423/hl3423_m.bnd.dcx"));
        assert!(!disc.exists("model/ac/parts/hand/hl3423/nope.bnd.dcx"));
        assert!(disc.read("nope/nope.bin").is_err());
        assert_eq!(&disc.head("param/accolor/color5001.bin", 4).unwrap(), &disc.read("param/accolor/color5001.bin").unwrap()[..4]);
        assert_eq!(disc.size("movie/jp/tu_boost.wmv").unwrap(), 2_505_347);
    }

    #[test]
    fn x360_disc_reads_unknown_names_by_hash() {
        let Some(disc) = iso() else { return };
        assert_eq!(unknown_hash("_unknown/param/1250250243.param"), Some(1250250243));
        assert_eq!(unknown_hash("param/1250250243.param"), None);
        let p = disc.read("_unknown/param/1250250243.param").unwrap();
        assert_eq!(p.len(), 87);
        assert_eq!(&p[0xc..0x25], b"EVENT_MESSAGE_TEXT_MAP_ST");
        assert!(disc.files().iter().any(|f| f == "_unknown/param/1250250243.param"));
    }

    #[test]
    fn x360_disc_reads_bundle_members() {
        let Some(disc) = iso() else { return };
        let list = disc.read("system/paramlist.xml").unwrap();
        assert_eq!(&list[..2], &[0xff, 0xfe]);
        assert_eq!(disc.size("system/paramlist.xml").unwrap(), list.len() as u64);
        assert!(disc.read("mission/ch3100.xml").is_ok());
        assert!(disc.files().iter().any(|f| f == "font/fontdef.xml"));
    }

    #[test]
    fn x360_disc_opens_binder_entries() {
        let Some(disc) = iso() else { return };
        let flv = disc.asset("model/ac/parts/arm/am0010/am0010_m.bnd.dcx|am0010.flv").unwrap();
        assert!(flv.starts_with(b"FLVER\0"));
    }

    #[test]
    fn x360_disc_lists_named_entries() {
        let Some(disc) = iso() else { return };
        let menus = disc.list("lang/en/menu");
        assert!(menus.iter().any(|m| m.eq_ignore_ascii_case("lang/en/menu/staffroll.drb.dcx")), "{menus:?}");
        assert!(menus.iter().all(|m| !m["lang/en/menu/".len()..].contains('/')));
        assert_eq!(disc.list("script/action/enemy").first().map(String::as_str), Some("script/action/enemy/e7010.lc"));
    }
}
