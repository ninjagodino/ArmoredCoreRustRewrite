//! `param/accolor/color%04d.bin` — per-design AC paint file (`0x358` = 856 bytes).
//!
//! Loaded by 360 `0x82331478` (`$(Data)\Param\AcColor\color%04d.bin`) via `0x82331680` and copied
//! into `assembly_data + 0xa0` by `0x82331b18` for `AcAssemblyDesignSt.coloring_id`.
//!
//! Layout (`0x82332420` init, `0x8235ddc8` compare, `0x82463630` unpack):
//! - `+0x000..0x330` (`816` bytes = `34 * 24`): 34 part-color groups (`groups[0..34]`), each a
//!   24-byte `ColoringSetSt` (`base`, `aid`, `aid2`, `optional`, `joint`, `kojima` as `[u8; 4]`
//!   RGBA, indexed at `base + group * 24` by `0x82463700`).
//! - `+0x330..0x352` (`34` bytes): 34 `u8` camouflage pattern IDs (`camo[0..34]`; `0` = none).
//! - `+0x352..0x354` (`2` bytes): zero padding.
//! - `+0x354..0x358` (`4` bytes): `[u8; 4]` RGBA `eye` color (channel `6` in `0x82463698` and
//!   `0x8257ff28`).

use anyhow::{ensure, Result};

pub const SIZE: usize = 0x358;
pub const GROUP_COUNT: usize = 34;
pub const GROUP_COLORS: usize = 6;
pub const OVERLAY_CHANNELS: usize = 7;

/// Channel indices within a 6-color group (`0x820223ac..0x820223d4`).
pub const CH_BASE: usize = 0;
pub const CH_AID: usize = 1;
pub const CH_AID2: usize = 2;
pub const CH_OPTIONAL: usize = 3;
pub const CH_JOINT: usize = 4;
pub const CH_KOJIMA: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcColor {
    /// 34 groups of 6 RGBA colors (`[base, aid, aid2, optional, joint, kojima]`).
    pub groups: [[[u8; 4]; GROUP_COLORS]; GROUP_COUNT],
    /// Camouflage pattern ID per group (`0` = none; `camo_id - 1` is passed to the composer).
    pub camo: [u8; GROUP_COUNT],
    /// Shared eye / glow RGBA color (`+0x354`, channel `6`).
    pub eye: [u8; 4],
}

pub fn read(data: &[u8]) -> Result<AcColor> {
    ensure!(
        data.len() == SIZE,
        "accolor is {} bytes, expected {SIZE} ({SIZE:#x})",
        data.len()
    );
    let mut groups = [[[0u8; 4]; GROUP_COLORS]; GROUP_COUNT];
    for (g, group) in groups.iter_mut().enumerate() {
        for (c, col) in group.iter_mut().enumerate() {
            let at = (g * GROUP_COLORS + c) * 4;
            col.copy_from_slice(&data[at..at + 4]);
        }
    }
    let mut camo = [0u8; GROUP_COUNT];
    camo.copy_from_slice(&data[0x330..0x352]);
    let mut eye = [0u8; 4];
    eye.copy_from_slice(&data[0x354..0x358]);
    Ok(AcColor { groups, camo, eye })
}

/// Convert one `[R, G, B, A]` byte color to `[f32; 4]`, remapping RGB into `[color_min, color_max]`
/// (360 `0x82460ef8`, reading `staticdrawparam.bin` fields 147 `color_min` at `+0x228` and 148
/// `color_max` at `+0x22c`).
pub fn rgba_to_float(rgba: [u8; 4], color_min: f32, color_max: f32) -> [f32; 4] {
    let span = color_max - color_min;
    let map = |b: u8| color_min + span * (f32::from(b) / 255.0);
    [
        map(rgba[0]),
        map(rgba[1]),
        map(rgba[2]),
        f32::from(rgba[3]) / 255.0,
    ]
}

impl AcColor {
    /// The 7 `FC_AC_OverlayCol` pixel-shader constants (`c180..c186`) for `group`, in the reversed
    /// order uploaded by 360 `0x82ca9c70` (`0x82bd6588`) and `0x82461ce0`:
    /// `[eye, kojima, joint, optional, aid2, aid, base]`.
    pub fn overlay_cols(&self, group: usize, color_min: f32, color_max: f32) -> [[f32; 4]; OVERLAY_CHANNELS] {
        let g = &self.groups[group.min(GROUP_COUNT - 1)];
        [
            rgba_to_float(self.eye, color_min, color_max),
            rgba_to_float(g[CH_KOJIMA], color_min, color_max),
            rgba_to_float(g[CH_JOINT], color_min, color_max),
            rgba_to_float(g[CH_OPTIONAL], color_min, color_max),
            rgba_to_float(g[CH_AID2], color_min, color_max),
            rgba_to_float(g[CH_AID], color_min, color_max),
            rgba_to_float(g[CH_BASE], color_min, color_max),
        ]
    }
}

/// Map an `assembly_slots.csv` slot name (and optional mesh root bone name) to its `AcColor` group
/// index (`0..12`), following 360 `0x83694720` (`0x838a2b58`), `0x82463750`, `0x82463798` and the
/// arm root split in `0x82b4d618` (`r_kata` -> 2 Right Arm, `l_kata` -> 3 Left Arm).
pub fn slot_group(slot: &str, root: Option<&str>) -> usize {
    match slot {
        "head" => 0,
        "core" | "booster" => 1,
        "arms" => match root {
            Some("l_kata") => 3,
            _ => 2,
        },
        "rack_r" => 2,
        "rack_l" => 3,
        "legs" => 4,
        "armwep_r" => 5,
        "armwep_l" => 6,
        "back_r" => 7,
        "back_l" => 8,
        "shoulder" => 9,
        "hanger_r" => 10,
        "hanger_l" => 11,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs;

    #[test]
    fn reads_all_disc_accolor_files() {
        let iso = vfs::repo_root().join(vfs::X360_ISO);
        if !iso.is_file() {
            return;
        }
        let disc = vfs::Disc::open(&iso).unwrap();
        let mut count = 0;
        for f in disc.files() {
            if f.starts_with("param/accolor/color") && f.ends_with(".bin") {
                let bytes = disc.read(&f).unwrap();
                let col = read(&bytes).unwrap_or_else(|e| panic!("{f}: {e}"));
                let ov = col.overlay_cols(0, 0.2, 0.8);
                for rgba in ov {
                    for ch in &rgba[..3] {
                        assert!((0.199..=0.801).contains(ch), "{f}: {ch}");
                    }
                }
                count += 1;
            }
        }
        assert!(count >= 200, "expected >= 200 accolor files, found {count}");
    }
}
