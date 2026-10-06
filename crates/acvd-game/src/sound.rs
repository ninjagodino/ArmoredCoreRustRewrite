//! The first free-play cues. The 360 build formats a name (`Sound_formatCue` would be
//! `FUN_82b47758`; the format table is at `0x8371c688`) and plays it through MagicOrchestra.
//! This plays the sample that name indexes (XMA or 16-bit PCM, `acvd_formats::fsb::Sample::decode`).
//! Cue rows are `sheets/sound_cues.csv`.
//!
//! Playback is not positional. `FUN_82b46df0` takes a position, and the follow camera sits
//! far enough out that Bevy's spatial rolloff would silence the source. Layer envelopes
//! (the speed crossfade on `b00000003`) are not applied: each layer's first sound plays
//! at full volume.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use acvd_data::generated::sound::PARAM_ACSOUNDPARAM_BIN;
use acvd_formats::vfs::Disc;
use acvd_formats::{fev, fsb};
use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings};
use bevy::prelude::*;

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
        let wav = match sample.decode().map(|pcm| wav_pcm(&pcm.to_i16(), pcm.channels, sample.frequency)) {
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
}
