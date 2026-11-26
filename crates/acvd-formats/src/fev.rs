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
    /// Authored event volume (linear gain, the FEV stores it converted from dB).
    pub volume: f32,
    /// Event mode flags (`0x0408_0010` on 3D events, `0x0018_0008` on 2D ones).
    pub mode: u32,
    /// 3D rolloff inner/outer distances (m); linear from `min_distance` to `max_distance`.
    pub min_distance: f32,
    pub max_distance: f32,
    pub layers: Vec<u16>,
    /// Every layer's full sound list and automation; `layers` is the first sound of each.
    pub mix: Vec<Layer>,
    /// Complex-event parameters (`speed`, `(distance)`, ...), referenced by `Layer::param`.
    pub params: Vec<Param>,
}

/// One complex-event layer. The sounds follow parameter `param` (`Event::params`).
#[derive(Debug, Clone, Default)]
pub struct Layer {
    /// Index into `Event::params` (`u16::MAX` when the header stored none).
    pub param: u16,
    pub sounds: Vec<SoundRef>,
    pub envelopes: Vec<Envelope>,
}

/// One sound entry of a layer: position/width along the layer parameter, its own volume and
/// start/stop fade times (seconds; -1 = engine default).
#[derive(Debug, Clone)]
pub struct SoundRef {
    pub def: u16,
    pub x: f32,
    pub width: f32,
    pub volume: f32,
    pub fade_in: f32,
    pub fade_out: f32,
}

/// A layer envelope over its parameter (x, value). `dsp` is the DSP parameter index:
/// `0xFFFF_FFFF` is a volume envelope, other values are DSP units (`ParamEQ`, pitch, ...).
#[derive(Debug, Clone)]
pub struct Envelope {
    pub dsp: u32,
    pub name: String,
    pub points: Vec<(f32, f32)>,
}

/// A layer's automation parameter (`speed`, `(distance)`, ...).
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    /// Top of the parameter range as authored (`150.0` for the booster `speed`).
    pub max: f32,
}

impl Event {
    /// 3D events attenuate by distance (FMOD linear rolloff, `min_distance`..`max_distance`).
    pub fn is_3d(&self) -> bool {
        self.mode & 0x0400_0000 != 0
    }

    /// Distance gain for a 3D event heard from `distance` metres: 1 at the inner distance,
    /// falling linearly to 0 at `max_distance`. 2D events always return 1.
    pub fn distance_gain(&self, distance: f32) -> f32 {
        if !self.is_3d() || self.max_distance <= self.min_distance {
            return 1.0;
        }
        ((self.max_distance - distance) / (self.max_distance - self.min_distance)).clamp(0.0, 1.0)
    }

    /// DSP-unit envelopes (non-volume), e.g. the speed `ParamEQ`.
    pub fn dsp_envelopes(&self) -> impl Iterator<Item = &Envelope> {
        self.mix
            .iter()
            .flat_map(|l| l.envelopes.iter())
            .filter(|e| !e.name.is_empty())
    }

    /// Plain volume envelopes of a layer, in file order (first sound's gain/fade, then the next).
    pub fn volume_envelopes(&self, layer: usize) -> Vec<&Envelope> {
        self.mix
            .get(layer)
            .map(|l| {
                l.envelopes
                    .iter()
                    .filter(|e| e.dsp == u32::MAX && e.name.is_empty() && e.points.len() > 1)
                    .collect()
            })
            .unwrap_or_default()
    }
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
        let Some(event) = self.event(name) else {
            return Vec::new();
        };
        event
            .layers
            .iter()
            .filter_map(|&i| self.defs.get(i as usize).and_then(|d| d.wave.as_ref()))
            .collect()
    }
}

pub fn is_fev(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

pub fn read(data: &[u8]) -> Result<Project> {
    ensure!(is_fev(data), "not an FEV1 project");
    let mut r = Le { d: data, at: 4 };
    let version = r.u32()?;
    ensure!(
        version == VERSION,
        "FEV version {version:#x}, want {VERSION:#x}"
    );
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
    Ok(Project {
        name,
        banks,
        events,
        defs,
    })
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
    let (volume, mode, min_distance, max_distance) = match event_header(r) {
        Ok(header) => header,
        Err(err) => bail!("{name}: {err}"),
    };
    let (layers, mix, params) = if kind & 0x18 == 0x08 {
        complex(r)?
    } else if kind & 0x18 == 0x10 {
        r.skip(4)?;
        let s = sound_ref(r)?;
        (
            vec![s.def],
            vec![Layer {
                param: 0,
                sounds: vec![s],
                envelopes: Vec::new(),
            }],
            Vec::new(),
        )
    } else {
        bail!("{name}: event kind {kind:#x}");
    };
    let n = r.u32()? as usize;
    for _ in 0..n {
        r.string()?;
    }
    Ok(Event {
        name,
        volume,
        mode,
        min_distance,
        max_distance,
        layers,
        mix,
        params,
    })
}

/// Event header. Returns the authored linear volume, mode flags and 3D rolloff distances.
fn event_header(r: &mut Le) -> Result<(f32, u32, f32, f32)> {
    // volume, pitch, pitch rand (v0x1B), volume rand (v0x20), priority (v0x0A), max playbacks,
    // steal priority (v0x38), mode + min/max distance.
    let volume = r.f32()?;
    r.skip(4 + 4 + 4 + 4 + 4 + 4)?;
    let mode = r.u32()?;
    let min_distance = r.f32()?;
    let max_distance = r.f32()?;
    // flags, speaker levels and cone (v0x09), playback flags + doppler (v0x0B),
    // reverb dry (v0x1C), reverb wet, speaker spread (v0x12), fades (v0x13),
    // spawn intensity and its rand (v0x2B, v0x2D), pan level (v0x16), position rand max (v0x28).
    r.skip(4 + 0x2C + 8 + 4 + 4 + 4 + 8 + 4 + 4 + 4 + 4)?;
    Ok((volume, mode, min_distance, max_distance))
}

fn complex(r: &mut Le) -> Result<(Vec<u16>, Vec<Layer>, Vec<Param>)> {
    let n = r.u32()? as usize;
    let mut layers = Vec::with_capacity(n);
    let mut mix = Vec::with_capacity(n);
    for _ in 0..n {
        // uses software, priority (v0x25), param index (v0x27).
        r.skip(4)?;
        let param = r.u16()?;
        let sounds = r.u16()? as usize;
        let envelopes = r.u16()? as usize;
        let mut refs = Vec::with_capacity(sounds);
        for _ in 0..sounds {
            refs.push(sound_ref(r)?);
        }
        let mut envs = Vec::with_capacity(envelopes);
        for _ in 0..envelopes {
            envs.push(envelope(r)?);
        }
        layers.push(refs.first().map(|s| s.def).unwrap_or(u16::MAX));
        mix.push(Layer {
            param,
            sounds: refs,
            envelopes: envs,
        });
    }
    let mut params = Vec::new();
    let nparams = r.u32()? as usize;
    for _ in 0..nparams {
        let name = r.string()?;
        // min, max, velocity, range min/max original, flags (0x18 bytes for v0x3D).
        r.skip(8)?;
        let max = r.f32()?;
        r.skip(12)?;
        // sustain-point count, then the points.
        let points = r.u32()? as usize;
        r.skip(points * 4)?;
        params.push(Param { name, max });
    }
    properties(r)?;
    Ok((layers, mix, params))
}

fn sound_ref(r: &mut Le) -> Result<SoundRef> {
    let def = r.u16()?;
    // x, width, flags, loop count, auto pitch, fine tune, volume and fades. See module docs.
    let x = r.f32()?;
    let width = r.f32()?;
    r.skip(4 + 4 + 4 + 8 + 4 + 4)?;
    let volume = r.f32()?;
    let fade_in = r.f32()?;
    let fade_out = r.f32()?;
    r.skip(8)?;
    Ok(SoundRef {
        def,
        x,
        width,
        volume,
        fade_in,
        fade_out,
    })
}

fn envelope(r: &mut Le) -> Result<Envelope> {
    let dsp = r.u32()?;
    let name = r.string()?;
    r.skip(12)?;
    let points = r.u32()? as usize;
    let mut pts = Vec::with_capacity(points);
    for _ in 0..points {
        pts.push((r.f32()?, r.f32()?));
        r.skip(4)?;
    }
    r.skip(8)?;
    Ok(Envelope {
        dsp,
        name,
        points: pts,
    })
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
    fn f32(&mut self) -> Result<f32> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes(b.try_into().unwrap()))
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
        ensure!(
            b.last() == Some(&0),
            "string at {:#x} is not NUL-terminated",
            self.at - n
        );
        Ok(String::from_utf8_lossy(&b[..n - 1]).into_owned())
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.d.len());
        let Some(end) = end else {
            bail!(
                "read of {n} bytes at {:#x} runs past end of {:#x}",
                self.at,
                self.d.len()
            );
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
        Some(
            crate::vfs::test_disc()?
                .read(&format!("sound/{name}"))
                .unwrap(),
        )
    }

    #[test]
    fn booster_events() {
        let Some(data) = disc("acv2_se_booster.fev") else {
            return;
        };
        let project = read(&data).unwrap();
        assert_eq!(project.name, "acv2_se_booster");
        assert_eq!(project.banks, ["se_booster"]);
        assert_eq!(project.events.len(), 9);
        assert_eq!(project.defs.len(), 8);
        let on = project.event("b00000000").unwrap();
        assert_eq!(on.layers, [3]);
        assert!((on.volume - 0.467735).abs() < 1e-5, "{}", on.volume);
        assert!(on.is_3d());
        assert!((on.min_distance - 0.0).abs() < 1e-6);
        assert!((on.max_distance - 100.0).abs() < 1e-4);
        // Two layers. The first holds two sounds (the speed crossfade); only its first
        // sound-def index is kept. The second layer is the high-cut.
        let speed = project.event("b00000003").unwrap();
        assert_eq!(speed.layers, [2, 5]);
        assert!((speed.volume - 0.316228).abs() < 1e-5, "{}", speed.volume);
        assert!(speed.is_3d());
        assert!((speed.min_distance - 0.0).abs() < 1e-6);
        assert!((speed.max_distance - 50.0).abs() < 1e-4);
        // Linear rolloff: unity at the inner distance, zero at the outer one.
        assert!((speed.distance_gain(0.0) - 1.0).abs() < 1e-6);
        assert!((speed.distance_gain(20.0) - 0.6).abs() < 1e-5);
        assert!((speed.distance_gain(50.0) - 0.0).abs() < 1e-6);
        assert!((speed.distance_gain(60.0) - 0.0).abs() < 1e-6);
        // The high-boost pair is mixed: b00000007 is 2D, its partner b00000008 is 3D.
        let quick = project.event("b00000007").unwrap();
        assert!(!quick.is_3d());
        assert_eq!(quick.distance_gain(500.0), 1.0);
        assert!(project.event("b00000008").unwrap().is_3d());
        assert_eq!(speed.mix.len(), 2);
        assert_eq!(speed.mix[0].param, 0);
        assert_eq!(speed.mix[0].sounds.len(), 2);
        let low = &speed.mix[0].sounds[0];
        let high = &speed.mix[0].sounds[1];
        assert_eq!(low.def, 2);
        assert_eq!(high.def, 1);
        assert!((low.x - 0.0).abs() < 1e-6 && (low.width - 0.561364).abs() < 1e-5);
        assert!((high.x - 0.303409).abs() < 1e-5 && (high.width - 0.696591).abs() < 1e-5);
        assert!((low.volume - 1.0).abs() < 1e-6);
        assert!((high.fade_in - 0.370311).abs() < 1e-5);
        assert!((low.fade_out - 0.459515).abs() < 1e-5);
        // The speed parameter is the layer parameter, and the layer envelopes are the
        // authored speed crossfade (two volume ramps, then per-sound fades).
        assert_eq!(speed.params[0].name, "speed");
        assert!((speed.params[0].max - 150.0).abs() < 1e-4);
        assert_eq!(speed.volume_envelopes(0).len(), 4);
        let env = speed.volume_envelopes(0)[0];
        assert_eq!(env.points[0], (0.0, 0.357699));
        assert_eq!(speed.volume_envelopes(0)[1].points[0], (0.0, 0.678201));
        assert_eq!(speed.mix[1].sounds.len(), 1);
        assert_eq!(speed.volume_envelopes(1).len(), 3);
        let wave = project.defs[3].wave.as_ref().unwrap();
        assert_eq!(wave.bank, "se_booster");
        assert_eq!(wave.index, 1);
        assert_eq!(project.waves("b00000010").len(), 1);
        assert_eq!(project.waves("b00000010")[0].index, 4);
    }

    #[test]
    fn weapon_cues() {
        let Some(data) = disc("acv2_se_weapon.fev") else {
            return;
        };
        let project = read(&data).unwrap();
        assert!(project.banks.iter().any(|b| b == "se_weapon"));
        for cue in ["w00000022", "w00000034", "w00009100"] {
            assert!(!project.waves(cue).is_empty(), "{cue} has no wave");
        }
    }

    #[test]
    fn ac_jump_cue() {
        let Some(data) = disc("acv2_se_ac.fev") else {
            return;
        };
        let project = read(&data).unwrap();
        let waves = project.waves("c00000024");
        assert_eq!(waves.len(), 1);
        assert_eq!(waves[0].bank, "se_ac");
    }
}
