//! The first free-play cues. The 360 build formats a name (`Sound_formatCue` would be
//! `FUN_82b47758`; the format table is at `0x8371c688`) and plays it through MagicOrchestra.
//! This plays the sample that name indexes: XMA on the 360 disc (`acvd_formats::xma`), MPEG on
//! the PS3 dump. Cue rows are `sheets/sound_cues.csv`.
//!
//! Playback is not positional. `FUN_82b46df0` takes a position, and the follow camera sits
//! far enough out that Bevy's spatial rolloff would silence the source. Layer envelopes
//! (the speed crossfade on `b00000003`) are not applied: each layer's first sound plays
//! at full volume.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::sync::Arc;

use acvd_data::generated::sound::PARAM_ACSOUNDPARAM_BIN;
use acvd_formats::vfs::Disc;
use acvd_formats::{fev, fsb};
use anyhow::{anyhow, bail, Context, Result};
use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings};
use bevy::prelude::*;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::control::{Pilot, Piloting};

/// Same gate as the main-booster VFX (`sfx::boosters`): boost mode and moving.
const BOOST_MOVING: f32 = 0.05;
/// `AcSfxCtrl` slot 2 (`0x82098cb0`, `FUN_82899280`) plays this on the emitter at object+0x124.
const BOOST_ON: &str = "b00000000";
/// Single-layer sustain in `acv2_se_booster.fev` (sound def → `se_booster` `main_boost11`).
/// Not a traced call: the speed-crossfade event `b00000003` is left unplayed.
const BOOST_LOOP: &str = "b00000010";

/// Projects and the banks a cue has already pulled in.
#[derive(Resource)]
pub struct Cues {
    disc: Disc,
    projects: Vec<fev::Project>,
    banks: HashMap<String, fsb::Bank>,
    decoded: HashMap<(String, u32), Handle<AudioSource>>,
    missing: HashSet<String>,
}

impl Cues {
    fn load(disc: &Disc) -> Self {
        let mut projects = Vec::new();
        for name in [
            "acv2_se_weapon.fev",
            "acv2_se_booster.fev",
            "acv2_se_ac.fev",
        ] {
            match disc
                .read(&format!("sound/{name}"))
                .and_then(|b| fev::read(&b))
            {
                Ok(project) => projects.push(project),
                Err(err) => eprintln!("sound: {err:#}"),
            }
        }
        Self {
            disc: disc.clone(),
            projects,
            banks: HashMap::new(),
            decoded: HashMap::new(),
            missing: HashSet::new(),
        }
    }

    fn play(
        &mut self,
        commands: &mut Commands,
        assets: &mut Assets<AudioSource>,
        cue: &str,
        looping: bool,
    ) -> Vec<Entity> {
        let waves: Vec<fev::Wave> = self
            .projects
            .iter()
            .find_map(|p| {
                p.event(cue)
                    .map(|_| p.waves(cue).into_iter().cloned().collect())
            })
            .unwrap_or_default();
        if waves.is_empty() {
            if self.missing.insert(cue.to_string()) {
                eprintln!("sound: no cue {cue}");
            }
            return Vec::new();
        }
        let settings = if looping {
            PlaybackSettings::LOOP
        } else {
            PlaybackSettings::DESPAWN
        };
        let mut spawned = Vec::new();
        for wave in waves {
            let Some(handle) = self.source(assets, &wave) else {
                continue;
            };
            spawned.push(commands.spawn((AudioPlayer(handle), settings)).id());
        }
        spawned
    }

    fn source(
        &mut self,
        assets: &mut Assets<AudioSource>,
        wave: &fev::Wave,
    ) -> Option<Handle<AudioSource>> {
        let key = (wave.bank.clone(), wave.index);
        if let Some(handle) = self.decoded.get(&key) {
            return Some(handle.clone());
        }
        let sample = self
            .bank(&wave.bank)
            .and_then(|b| b.samples.get(wave.index as usize));
        let Some(sample) = sample else {
            let name = format!("{}#{}", wave.bank, wave.index);
            if self.missing.insert(name.clone()) {
                eprintln!("sound: {name} is not in the bank");
            }
            return None;
        };
        let decoded = if sample.mpeg() {
            mpeg_to_wav(&sample.data)
        } else {
            sample.decode().map(|pcm| wav_pcm(&pcm.to_i16(), pcm.channels, sample.frequency))
        };
        let wav = match decoded {
            Ok(wav) => wav,
            Err(err) => {
                eprintln!("sound: {}#{}: {err:#}", wave.bank, wave.index);
                return None;
            }
        };
        let handle = assets.add(AudioSource {
            bytes: Arc::<[u8]>::from(wav),
        });
        self.decoded.insert(key, handle.clone());
        Some(handle)
    }

    fn bank(&mut self, name: &str) -> Option<&fsb::Bank> {
        if !self.banks.contains_key(name) {
            match self
                .disc
                .read(&format!("sound/{name}.fsb"))
                .and_then(|b| fsb::read(&b))
            {
                Ok(bank) => {
                    self.banks.insert(name.to_string(), bank);
                }
                Err(err) => {
                    if self.missing.insert(name.to_string()) {
                        eprintln!("sound: {err:#}");
                    }
                    return None;
                }
            }
        }
        self.banks.get(name)
    }
}

/// One weapon shot. `id` is `acweaponsoundparam.shoot`; category 4 formats it `w%08d`
/// (`FUN_828995d0`). Zero and negative play nothing.
pub fn shot(commands: &mut Commands, cues: &mut Cues, assets: &mut Assets<AudioSource>, id: i16) {
    if id > 0 {
        cues.play(commands, assets, &format!("w{id:08}"), false);
    }
}

pub struct SoundPlugin {
    pub disc: Disc,
}

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Cues::load(&self.disc))
            .add_systems(Update, motion.after(crate::weapons::fire));
    }
}

#[derive(Default)]
struct Heard {
    moving: bool,
    airborne: bool,
    sustain: Vec<Entity>,
}

/// Boost start, boost sustain, and jump. Runs after the pilot step that sets those flags.
fn motion(
    mut commands: Commands,
    mut cues: ResMut<Cues>,
    mut assets: ResMut<Assets<AudioSource>>,
    piloting: Res<Piloting>,
    pilots: Query<&Pilot>,
    mut heard: Local<Heard>,
) {
    let Ok(pilot) = pilots.single() else { return };
    if !piloting.0 {
        stop(&mut commands, &mut heard.sustain);
        heard.moving = false;
        heard.airborne = pilot.airborne;
        return;
    }
    let flat = Vec2::new(pilot.velocity.x, pilot.velocity.z);
    let moving = pilot.boost && flat.length() > BOOST_MOVING;
    if moving && !heard.moving {
        cues.play(&mut commands, &mut assets, BOOST_ON, false);
    }
    if moving && heard.sustain.is_empty() {
        heard.sustain = cues.play(&mut commands, &mut assets, BOOST_LOOP, true);
    } else if !moving {
        stop(&mut commands, &mut heard.sustain);
    }
    heard.moving = moving;

    // Rising edge of airborne with upward speed: a jump. A ledge fall leaves `vy` ≤ 0.
    // `se_jump` is +0xA of `AC_SOUNDPARAM_ST` (`FUN_82899168`, category 1 `c%08d`). Row 0
    // stands in for the AC's own row; every row stores the same id.
    let jump = pilot.airborne && !heard.airborne && pilot.velocity.y > 0.0;
    heard.airborne = pilot.airborne;
    if jump {
        if let Some(row) = acvd_data::find(PARAM_ACSOUNDPARAM_BIN, 0).filter(|r| r.data.se_jump > 0)
        {
            cues.play(
                &mut commands,
                &mut assets,
                &format!("c{:08}", row.data.se_jump),
                false,
            );
        }
    }
}

fn stop(commands: &mut Commands, entities: &mut Vec<Entity>) {
    for entity in entities.drain(..) {
        commands.entity(entity).try_despawn();
    }
}

/// MPEG frames (`FF FB` …) to a 16-bit PCM WAV. Bevy's default audio feature decodes
/// vorbis; `wav` is enabled on this crate so rodio can play the result.
pub(crate) fn mpeg_to_wav(data: &[u8]) -> Result<Vec<u8>> {
    // FMOD pads every MPEG frame up to a multiple of 4. A stock decoder looks for the next
    // frame at the unpadded size and never locks.
    let frames = unpad_mpeg(data)?;
    let source =
        MediaSourceStream::new(Box::new(MpegBytes(Cursor::new(frames))), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| anyhow!("probe: {e}"))?;
    let mut format = probed.format;
    let (track_id, params) = {
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
            .context("no audio track")?;
        (track.id, track.codec_params.clone())
    };
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| anyhow!("decoder: {e}"))?;
    let mut pcm = Vec::new();
    let mut spec = None;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(err) => return Err(anyhow!("packet: {err}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder
            .decode(&packet)
            .map_err(|e| anyhow!("decode: {e}"))?;
        if spec.is_none() {
            spec = Some(*decoded.spec());
        }
        let mut buf = SampleBuffer::<i16>::new(decoded.capacity() as u64, *decoded.spec());
        buf.copy_interleaved_ref(decoded);
        pcm.extend_from_slice(buf.samples());
    }
    let spec = spec.context("no frames")?;
    let channels = u16::try_from(spec.channels.count()).context("channel count")?;
    if channels == 0 || spec.rate == 0 || pcm.is_empty() {
        bail!("empty audio");
    }
    Ok(wav_pcm(&pcm, channels, spec.rate))
}

/// Copies each MPEG frame and drops the 0–3 padding bytes FMOD inserts so the next frame
/// starts on a 4-byte boundary.
fn unpad_mpeg(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut at = 0;
    while at + 4 <= data.len() {
        let header = u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
        let Some(len) = mpeg_frame_len(header) else {
            if out.is_empty() {
                bail!("mpeg frame at {at:#x} is not a frame header");
            }
            break;
        };
        if at + len > data.len() {
            break;
        }
        out.extend_from_slice(&data[at..at + len]);
        let padded = (len + 3) & !3;
        at = if at + len < data.len() && data[at + len] == 0xFF {
            at + len
        } else if at + padded <= data.len() {
            at + padded
        } else {
            break;
        };
    }
    if out.is_empty() {
        bail!("no mpeg frames");
    }
    Ok(out)
}

fn mpeg_frame_len(header: u32) -> Option<usize> {
    let version = (header >> 19) & 3;
    let layer = (header >> 17) & 3;
    let bitrate_i = ((header >> 12) & 0xF) as usize;
    let rate_i = ((header >> 10) & 3) as usize;
    let pad = ((header >> 9) & 1) as usize;
    if version == 1 || layer == 0 || bitrate_i == 0 || bitrate_i == 15 || rate_i == 3 {
        return None;
    }
    let mpeg1 = version == 3;
    let rate = match (version, rate_i) {
        (3, 0) => 44_100,
        (3, 1) => 48_000,
        (3, 2) => 32_000,
        (2, 0) => 22_050,
        (2, 1) => 24_000,
        (2, 2) => 16_000,
        (0, 0) => 11_025,
        (0, 1) => 12_000,
        (0, 2) => 8_000,
        _ => return None,
    };
    // Layer field: 1 = III, 2 = II, 3 = I. Values are kbit/s.
    let kbps: &[u32] = match (mpeg1, layer) {
        (true, 1) => &[
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
        ],
        (false, 1) => &[
            0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
        ],
        (true, 2) => &[
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0,
        ],
        (false, 2) => &[
            0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
        ],
        (true, 3) => &[
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 0,
        ],
        (false, 3) => &[
            0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256, 0,
        ],
        _ => return None,
    };
    let factor = match (layer, mpeg1) {
        (3, _) => 12,
        (1, true) => 144,
        (1, false) => 72,
        (2, _) => 144,
        _ => return None,
    };
    let slots = factor * kbps[bitrate_i] * 1000 / rate + pad as u32;
    Some(if layer == 3 {
        slots as usize * 4
    } else {
        slots as usize
    })
}

/// Reports its length. The MPEG probe treats a stream with an unknown length as empty.
struct MpegBytes(Cursor<Vec<u8>>);

impl Read for MpegBytes {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}
impl Seek for MpegBytes {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}
impl MediaSource for MpegBytes {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.0.get_ref().len() as u64)
    }
}

fn wav_pcm(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
    let data_bytes = samples.len() * 2;
    let block = u32::from(channels) * 2;
    let mut out = Vec::with_capacity(44 + data_bytes);
    out.extend(b"RIFF");
    out.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * block).to_le_bytes());
    out.extend_from_slice(&(block as u16).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend(b"data");
    out.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(name: &str) -> Option<Vec<u8>> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("ACVD Unbound")
            .join("PS3_GAME/USRDIR/sound")
            .join(name);
        std::fs::read(path).ok()
    }

    fn peak(wav: &[u8]) -> u16 {
        wav[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
            .fold(0, u16::max)
    }

    /// The same cues from the 360 ISO: XMA samples decode, at the PS3 bank's sample names.
    #[test]
    fn gameplay_cues_decode_x360() {
        let iso = acvd_formats::vfs::repo_root().join(acvd_formats::vfs::X360_ISO);
        let Ok(disc) = Disc::open(&iso) else { return };
        let mut cues = Cues::load(&disc);
        assert_eq!(cues.projects.len(), 3);
        for cue in ["w00000034", BOOST_ON, BOOST_LOOP, "c00000024"] {
            let waves: Vec<fev::Wave> = cues.projects.iter().find_map(|p| p.event(cue).map(|_| p.waves(cue).into_iter().cloned().collect())).unwrap();
            let wave = &waves[0];
            let sample = cues.bank(&wave.bank).unwrap().samples[wave.index as usize].clone();
            assert!(sample.xma(), "{cue}: {} mode {:#x}", sample.name, sample.mode);
            let pcm = sample.decode().unwrap();
            assert_eq!(pcm.errors, 0, "{cue}");
            assert_eq!(pcm.samples.len(), sample.length as usize * sample.channels as usize);
            assert!(pcm.to_i16().iter().any(|&s| s.unsigned_abs() > 1000), "{cue} decoded to silence");
        }
    }

    #[test]
    fn gameplay_cues_decode() {
        let Some(weapon_fev) = disc("acv2_se_weapon.fev") else {
            return;
        };
        let Some(booster_fev) = disc("acv2_se_booster.fev") else {
            return;
        };
        let Some(ac_fev) = disc("acv2_se_ac.fev") else {
            return;
        };
        let weapon = fev::read(&weapon_fev).unwrap();
        let booster = fev::read(&booster_fev).unwrap();
        let ac = fev::read(&ac_fev).unwrap();
        for (project, cue) in [
            (&weapon, "w00000034"),
            (&booster, BOOST_ON),
            (&booster, BOOST_LOOP),
            (&ac, "c00000024"),
        ] {
            let wave = project
                .waves(cue)
                .into_iter()
                .next()
                .cloned()
                .unwrap_or_else(|| panic!("{cue} has no wave"));
            let file = disc(&format!("{}.fsb", wave.bank))
                .unwrap_or_else(|| panic!("missing {}", wave.bank));
            let bank = fsb::read(&file).unwrap();
            let sample = &bank.samples[wave.index as usize];
            let wav = mpeg_to_wav(&sample.data).unwrap_or_else(|e| panic!("{cue}: {e:#}"));
            assert!(
                wav.starts_with(b"RIFF") && peak(&wav) > 0,
                "{cue} decoded to silence"
            );
        }
    }
}
