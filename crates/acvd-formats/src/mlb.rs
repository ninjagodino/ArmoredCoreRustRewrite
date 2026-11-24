//! Model Layout Binary (`*_m.mlb`, inside `model/obj/o####/o####_m.bnd.dcx`).
//!
//! Carries the per-object `DestroyApSt` (`param/destroyap.param`) row id for map objects
//! (`360 0x82c02c58` -> `0x82c01e50` -> `0x82c02160` -> `0x82c01de8`/`0x82c01e00`, param `0x2e`).

use anyhow::{ensure, Result};

use crate::reader::Be;

/// Decoded destruction durability info from an object's `*_m.mlb` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestroyApIds {
    /// Object-wide default `DestroyApSt` id (at header `+0x2c`, `0x82c02c74`).
    pub default_id: Option<u16>,
    /// Per-subpart `DestroyApSt` ids (`0x82c01e50`, overriding `default_id` when nonzero).
    pub sub_ids: Vec<u16>,
}

impl DestroyApIds {
    /// Effective `DestroyApSt` id for the object (`0x82c02160` -> `0x82c01e00` / `0x82c01de8`):
    /// ignores `0` and `0xFFFF` (inherit default) and prefers a destructible subpart id (`!= 1`,
    /// where `1` is `【デフォルト】絶対壊れない`) over an indestructible base, falling back to
    /// `default_id`.
    pub fn primary_id(&self) -> Option<u16> {
        let valid = |&id: &u16| id != 0 && id != u16::MAX;
        self.sub_ids
            .iter()
            .copied()
            .find(|id| valid(id) && *id != 1)
            .or_else(|| self.default_id.filter(|id| valid(id) && *id != 1))
            .or_else(|| self.sub_ids.iter().copied().find(valid))
            .or_else(|| self.default_id.filter(valid))
    }
}

/// Reads the `DestroyApSt` ids from a raw `*_m.mlb` file (big-endian Xbox 360).
///
/// Header (`0x82c02c58` / `0x82c01e50`):
/// - `+0x00`: `u32` offset to section offset table (`0x10`)
/// - `+0x04`: `u32` section count
/// - Each section header (`sec_off`):
///   - `+0x00`: `u32` section type (`0x80` = model/destroy descriptor with `+0x18/+0x1c/+0x2c`)
///   - `+0x18`: `u32` absolute offset to subpart pointer table (`count` x `u32` absolute offsets)
///   - `+0x1c`: `u32` subpart count
///   - `+0x2c`: `u32` relative offset from `sec_off` to default `u16` `DestroyApSt` id
///   - Each subpart entry (`entry_off`) has `+0x0c`: `u32` relative offset from `entry_off` to
///     per-subpart `u16` `DestroyApSt` id (overrides default when nonzero).
pub fn read_destroy_ap_ids(data: &[u8]) -> Result<DestroyApIds> {
    let r = Be(data);
    ensure!(data.len() >= 0x30, "MLB too short ({} bytes)", data.len());
    let table_off = r.u32(0x00)? as usize;
    let count = r.u32(0x04)? as usize;
    let mut default_id = None;
    let mut sub_ids = Vec::new();
    if count <= 64 && table_off + count * 4 <= data.len() {
        for i in 0..count {
            let sec_off = r.u32(table_off + i * 4)? as usize;
            if sec_off == 0 || sec_off + 0x30 > data.len() {
                continue;
            }
            // Look for the destroy-capable section (type 0x80 = `0x82c572e8(..., 2)` / `0x82c02c58`):
            let sec_type = r.u32(sec_off)?;
            if sec_type != 0x80 {
                continue;
            }
            let def_rel = r.u32(sec_off + 0x2c)? as usize;
            if def_rel > 0 && sec_off + def_rel + 2 <= data.len() {
                let id = r.u16(sec_off + def_rel)?;
                if id != 0 {
                    default_id = Some(id);
                }
            }
            let sub_table = r.u32(sec_off + 0x18)? as usize;
            let sub_count = r.u32(sec_off + 0x1c)? as usize;
            if sub_table > 0 && sub_count <= 256 && sub_table + sub_count * 4 <= data.len() {
                for s in 0..sub_count {
                    let entry_off = r.u32(sub_table + s * 4)? as usize;
                    if entry_off == 0 || entry_off + 0x10 > data.len() {
                        continue;
                    }
                    let ap_rel = r.u32(entry_off + 0x0c)? as usize;
                    if ap_rel > 0 && entry_off + ap_rel + 2 <= data.len() {
                        let id = r.u16(entry_off + ap_rel)?;
                        if id != 0 {
                            sub_ids.push(id);
                        }
                    }
                }
            }
        }
    }
    Ok(DestroyApIds {
        default_id,
        sub_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs;

    #[test]
    fn m4000_destructible_mlb_ids() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = vfs::Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        let lookup = |model: &str| {
            let path = format!("model/obj/{model}/{model}_m.bnd.dcx|{model}_m.mlb");
            let bytes = vfs::open(&disc, &path).unwrap();
            read_destroy_ap_ids(&bytes).unwrap().primary_id()
        };
        // 耐久A (AP=1, collision_defensibility=0)
        assert_eq!(lookup("o2024"), Some(100));
        assert_eq!(lookup("o2070"), Some(100));
        assert_eq!(lookup("o4235"), Some(100));
        // 耐久B (AP=500, collision_defensibility=0)
        assert_eq!(lookup("o2037"), Some(101));
        assert_eq!(lookup("o0006"), Some(101));
        // 【デフォルト】絶対壊れない (cutoff=65535, collision_defensibility=100)
        assert_eq!(lookup("o2106"), Some(1));
        assert_eq!(lookup("o2107"), Some(1));
    }
}
