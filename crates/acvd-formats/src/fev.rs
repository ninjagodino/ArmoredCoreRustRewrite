//! FEV1 event project (`sound/*.fev`, magic `FEV1`). Little-endian FMOD Designer project,
//! version `0x003D0000` (FMOD Designer 61, the version vgmstream tags as Armored Core V).
//!
//! The file is a sequence with no pointers: project name, wave banks, a category tree,
//! event groups, sound-def templates, then sound defs. An event names a cue (`w00000022`);
//! each of its layers holds a sound-def index, and that def's first wavetable entry is a
//! wave-bank name plus a sample index into the matching FSB. Later chunks (reverb,
//! composition) are not read.
//!
//! Field skips follow the version gates in vgmstream `src/meta/fsb_fev.h` for `0x3D`. The
//! cue-name formats that index these events are in `sheets/sound_cues.csv`.

use anyhow::{bail, ensure, Result};

pub const MAGIC: &[u8] = b"FEV1";
/// Armored Core V / Verdict Day event projects.
pub const VERSION: u32 = 0x003D_0000;

#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub banks: Vec<String>,
    pub events: Vec<Event>,
    pub defs: Vec<SoundDef>,
}

/// One cue. `layers` are sound-def indices, one per layer (that layer's first sound).
#[derive(Debug, Clone)]
pub struct Event {
    pub name: String,
    pub layers: Vec<u16>,
}

/// A sound definition. `wave` is its first wavetable entry.
#[derive(Debug, Clone)]
pub struct SoundDef {
    pub name: String,
    pub wave: Option<Wave>,
}

#[derive(Debug, Clone)]
pub struct Wave {
    pub bank: String,
    pub index: u32,
}

impl Project {
    pub fn event(&self, name: &str) -> Option<&Event> {
        self.events.iter().find(|e| e.name == name)
    }

    /// First wavetable entry of each layer, in layer order.
    pub fn waves(&self, name: &str) -> Vec<&Wave> {
        let Some(event) = self.event(name) else { return Vec::new() };
        event.layers.iter().filter_map(|&i| self.defs.get(i as usize).and_then(|d| d.wave.as_ref())).collect()
    }
}

pub fn is_fev(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

pub fn read(data: &[u8]) -> Result<Project> {
    ensure!(is_fev(data), "not an FEV1 project");
    let mut r = Le { d: data, at: 4 };
    let version = r.u32()?;
    ensure!(version == VERSION, "FEV version {version:#x}, want {VERSION:#x}");
    // v0x2E sound-def pool, v0x32 64-bit pool. The v0x40 object table is absent before 0x41.
    r.skip(8)?;
    let name = r.string()?;
    let banks_n = r.u32()? as usize;
    let mut banks = Vec::with_capacity(banks_n);
    for _ in 0..banks_n {
        // mode, max streams (v0x14), per-language hash (v0x3D, one language).
        r.skip(4 + 4 + 8)?;
        banks.push(r.string()?);
    }
    category(&mut r)?;
    let groups = r.u32()? as usize;
    let mut events = Vec::new();
    for _ in 0..groups {
        event_group(&mut r, &mut events)?;
    }
    let templates = r.u32()? as usize;
    for _ in 0..templates {
        // Sound-def template body for version 0x3D (vgmstream `parse_fev_sound_def_def`).
        r.skip(0x40)?;
    }
    let defs_n = r.u32()? as usize;
    let mut defs = Vec::with_capacity(defs_n);
    for _ in 0..defs_n {
        defs.push(sound_def(&mut r)?);
    }
    Ok(Project { name, banks, events, defs })
}

fn category(r: &mut Le) -> Result<()> {
    r.string()?;
    // volume, pitch, and (v0x29) max playbacks plus flags.
    r.skip(16)?;
    let n = r.u32()? as usize;
    for _ in 0..n {
        category(r)?;
    }
    Ok(())
}

fn event_group(r: &mut Le, events: &mut Vec<Event>) -> Result<()> {
    r.string()?;
    properties(r)?;
    let groups = r.u32()? as usize;
    let count = r.u32()? as usize;
    for _ in 0..groups {
        event_group(r, events)?;
    }
    for _ in 0..count {
        events.push(event(r)?);
    }
    Ok(())
}

fn event(r: &mut Le) -> Result<Event> {
    let kind = r.u32()?;
    let name = r.string()?;
    r.skip(16)?; // UUID, present from v0x3A
    if let Err(err) = skip_event_header(r) {
        bail!("{name}: {err}");
    }
    let layers = if kind & 0x18 == 0x08 {
        complex(r)?
    } else if kind & 0x18 == 0x10 {
        r.skip(4)?;
        vec![sound_ref(r)?]
    } else {
        bail!("{name}: event kind {kind:#x}");
    };
    let n = r.u32()? as usize;
    for _ in 0..n {
        r.string()?;
    }
    Ok(Event { name, layers })
}

fn skip_event_header(r: &mut Le) -> Result<()> {
    // volume, pitch, pitch rand (v0x1B), volume rand (v0x20), priority (v0x0A), max playbacks,
    // steal priority (v0x38), mode + min/max distance.
    r.skip(8 + 4 + 4 + 4 + 4 + 4 + 12)?;
    // flags, speaker levels and cone (v0x09), playback flags + doppler (v0x0B),
    // reverb dry (v0x1C), reverb wet, speaker spread (v0x12), fades (v0x13),
    // spawn intensity and its rand (v0x2B, v0x2D), pan level (v0x16), position rand max (v0x28).
    r.skip(4 + 0x2C + 8 + 4 + 4 + 4 + 8 + 4 + 4 + 4 + 4)?;
    Ok(())
}

fn complex(r: &mut Le) -> Result<Vec<u16>> {
    let n = r.u32()? as usize;
    let mut layers = Vec::with_capacity(n);
    for _ in 0..n {
        // flags, priority (v0x25), param index (v0x27).
        r.skip(6)?;
        let sounds = r.u16()? as usize;
        let envelopes = r.u16()? as usize;
        let first = (sounds > 0).then(|| sound_ref(r)).transpose()?;
        for _ in 1..sounds {
            sound_ref(r)?;
        }
        for _ in 0..envelopes {
            envelope(r)?;
        }
        if let Some(index) = first {
            layers.push(index);
        }
    }
    let params = r.u32()? as usize;
    for _ in 0..params {
        r.string()?;
        // velocity through flags, seek speed (v0x12), then the sustain-point count.
        r.skip(0x18)?;
        let points = r.u32()? as usize;
        r.skip(points * 4)?;
    }
    properties(r)?;
    Ok(layers)
}

fn sound_ref(r: &mut Le) -> Result<u16> {
    let index = r.u16()?;
    // x, width, flags, loop count, auto pitch, fine tune, volume and fades. See module docs.
    r.skip(0x38)?;
    Ok(index)
}

fn envelope(r: &mut Le) -> Result<()> {
    r.skip(4)?;
    r.string()?;
    r.skip(12)?;
    let points = r.u32()? as usize;
    r.skip(points * 12 + 8)?;
    Ok(())
}

fn properties(r: &mut Le) -> Result<()> {
    let n = r.u32()? as usize;
    for _ in 0..n {
        r.string()?;
        match r.u32()? {
            0 | 1 => r.skip(4)?,
            2 => {
                r.string()?;
            }
            other => bail!("property type {other} at {:#x}", r.at),
        }
    }
    Ok(())
}

fn sound_def(r: &mut Le) -> Result<SoundDef> {
    let name = r.string()?;
    r.skip(4)?;
    let n = r.u32()? as usize;
    let mut wave = None;
    for _ in 0..n {
        let kind = r.u32()?;
        r.skip(4)?;
        match kind {
            0 => {
                r.string()?;
                let bank = r.string()?;
                let index = r.u32()?;
                r.skip(4)?;
                if wave.is_none() {
                    wave = Some(Wave { bank, index });
                }
            }
            1 => r.skip(8)?,
            2 | 3 => {}
            other => bail!("{name}: sound entry {other:#x}"),
        }
    }
    Ok(SoundDef { name, wave })
}

struct Le<'a> {
    d: &'a [u8],
    at: usize,
}

impl<'a> Le<'a> {
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes(b.try_into().unwrap()))
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
    fn string(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        ensure!(n < 0x400, "string length {n:#x} at {:#x}", self.at);
        if n == 0 {
            return Ok(String::new());
        }
        let b = self.take(n)?;
        ensure!(b.last() == Some(&0), "string at {:#x} is not NUL-terminated", self.at - n);
        Ok(String::from_utf8_lossy(&b[..n - 1]).into_owned())
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.d.len());
        let Some(end) = end else {
            bail!("read of {n} bytes at {:#x} runs past end of {:#x}", self.at, self.d.len());
        };
        let b = &self.d[self.at..end];
        self.at = end;
        Ok(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(name: &str) -> Option<Vec<u8>> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join("ACVD Unbound").join("PS3_GAME/USRDIR/sound").join(name);
        std::fs::read(path).ok()
    }

    #[test]
    fn booster_events() {
        let Some(data) = disc("acv2_se_booster.fev") else { return };
        let project = read(&data).unwrap();
        assert_eq!(project.name, "acv2_se_booster");
        assert_eq!(project.banks, ["se_booster"]);
        assert_eq!(project.events.len(), 9);
        assert_eq!(project.defs.len(), 8);
        assert_eq!(project.event("b00000000").unwrap().layers, [3]);
        // Two layers. The first holds two sounds (the speed crossfade); only its first
        // sound-def index is kept. The second layer is the high-cut.
        assert_eq!(project.event("b00000003").unwrap().layers, [2, 5]);
        let wave = project.defs[3].wave.as_ref().unwrap();
        assert_eq!(wave.bank, "se_booster");
        assert_eq!(wave.index, 1);
        assert_eq!(project.waves("b00000010").len(), 1);
        assert_eq!(project.waves("b00000010")[0].index, 4);
    }

    #[test]
    fn weapon_cues() {
        let Some(data) = disc("acv2_se_weapon.fev") else { return };
        let project = read(&data).unwrap();
        assert!(project.banks.iter().any(|b| b == "se_weapon"));
        for cue in ["w00000022", "w00000034", "w00009100"] {
            assert!(!project.waves(cue).is_empty(), "{cue} has no wave");
        }
    }

    #[test]
    fn ac_jump_cue() {
        let Some(data) = disc("acv2_se_ac.fev") else { return };
        let project = read(&data).unwrap();
        let waves = project.waves("c00000024");
        assert_eq!(waves.len(), 1);
        assert_eq!(waves[0].bank, "se_ac");
    }
}
