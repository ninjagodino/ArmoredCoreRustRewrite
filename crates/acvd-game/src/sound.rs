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
use std::sync::{mpsc, Arc, Mutex};

use acvd_data::generated::sound::PARAM_ACSOUNDPARAM_BIN;
use acvd_formats::vfs::Disc;
use acvd_formats::{env, fev, fsb};
use bevy::audio::{
    AudioPlayer, AudioSink, AudioSinkPlayback, AudioSource, GlobalVolume, PlaybackSettings, Volume,
};
use bevy::prelude::*;

use crate::control::{Pilot, Piloting};

/// Same gate as the main-booster VFX (`sfx::boosters`): boost mode and moving.
const BOOST_MOVING: f32 = 0.05;
/// `AcSfxCtrl` slot 2 (`0x82098cb0`, `FUN_82899280`) plays this on the emitter at object+0x124.
const BOOST_ON: &str = "b00000000";
/// Single-layer sustain in `acv2_se_booster.fev` (sound def → `se_booster` `main_boost11`).
/// Not a traced call: the speed-crossfade event `b00000003` is left unplayed.
const BOOST_LOOP: &str = "b00000010";
/// `AcSfxCtrl` slot +0x40 plays both (`boost_on06` + `boost11`, and `quick_boost03`): high boost.
const QUICK_BOOST: [&str; 2] = ["b00000007", "b00000008"];
/// `AcSfxCtrl` slots +0x30 / +0x3c (`dash00` + `boost_on06`), taken as the glide start; not traced
/// to the glide caller.
const GLIDE_ON: &str = "b00000006";

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
            "acv2_se_bombs.fev",
            "acv2_se_booster.fev",
            "acv2_se_ac.fev",
            "acv2_se_enemy.fev",
            "acv2_se_menu.fev",
            "acv2_se_garage.fev",
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
        let wav = match sample
            .decode()
            .map(|pcm| wav_pcm(&pcm.to_i16(), pcm.channels, sample.frequency))
        {
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

/// Plays a named one-shot cue (e.g. `w00009224` bullet hit, `w00009600` explosion, `c00000089` AC death).
pub fn cue(commands: &mut Commands, cues: &mut Cues, assets: &mut Assets<AudioSource>, name: &str) {
    if !name.is_empty() {
        cues.play(commands, assets, name, false);
    }
}

/// Default BGM volume (keeps SFX clearly audible over music).
pub(crate) const BGM_VOLUME: f32 = 0.35;

/// Absolute BGM sink volume for the current mute + master-volume pair.
pub(crate) fn bgm_playback_volume(muted: bool, master: f32) -> Volume {
    if muted {
        Volume::Linear(0.0)
    } else {
        Volume::Linear(BGM_VOLUME * master.clamp(0.0, 1.0))
    }
}

pub struct SoundPlugin {
    pub disc: Disc,
    /// Map id (e.g. `"m4000"`) whose `ch_env/{map}_env.msb` selects the BGM track, if any.
    pub map: Option<String>,
    /// False in headless `--shot` runs to skip spawning a background BGM decode thread.
    pub play_bgm: bool,
}

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        let bgm = if self.play_bgm {
            BgmState::spawn_decode(&self.disc, self.map.as_deref())
        } else {
            BgmState::default()
        };
        app.insert_resource(Cues::load(&self.disc))
            .insert_resource(bgm)
            .add_systems(Update, (motion.after(crate::weapons::fire), bgm_update));
    }
}

#[derive(Component)]
pub struct BgmPlayer;

#[derive(Resource, Default)]
pub struct BgmState {
    rx: Option<Mutex<mpsc::Receiver<Vec<u8>>>>,
    muted: bool,
    enabled: bool,
}

/// Switches the active looping BGM track (`None` plays the garage theme `grg` from
/// `acv2_bgm_menu_bgm_menu.fev`; `Some(map)` plays that map's `EVENT_BGM` from `acv2_bgm_bgm.fev`).
pub fn switch_bgm(
    commands: &mut Commands,
    bgm: &mut BgmState,
    players: &Query<Entity, With<BgmPlayer>>,
    disc: &Disc,
    map: Option<&str>,
) {
    for entity in players {
        commands.entity(entity).despawn();
    }
    if !bgm.enabled {
        bgm.rx = None;
        return;
    }
    let muted = bgm.muted;
    *bgm = BgmState::spawn_decode(disc, map);
    bgm.muted = muted;
}

/// Resolves the BGM wave for `map` from `ch_env/{map}_env.msb` (`EVENT_BGM`, 360 `0x82800128`)
/// and `sound/acv2_bgm_bgm.fev` (falling back to the map's 3-digit arena/mission track or
/// `acv2_bgm_menu_bgm_menu.fev` `grg` when no map is loaded).
fn resolve_bgm_wave(disc: &Disc, map: Option<&str>) -> Option<(String, fev::Wave)> {
    if let Some(map_id) = map {
        if let Ok(project) = disc
            .read("sound/acv2_bgm_bgm.fev")
            .and_then(|b| fev::read(&b))
        {
            let mut candidates = Vec::new();
            let env_path = format!("model/map/ch_env/{map_id}_env.msb");
            if let Ok(e) = disc.read(&env_path).and_then(|b| env::read(&b)) {
                if let Some(bgm) = e.bgm.filter(|b| b.id > 0) {
                    candidates.push(format!("{:03}", bgm.id));
                }
            }
            if let Some(digits) = map_id.strip_prefix('m').and_then(|s| s.get(..3)) {
                candidates.push(digits.to_string());
            }
            candidates.push("400".to_string());
            for cue in candidates {
                if let Some(wave) = project.waves(&cue).into_iter().next().cloned() {
                    return Some((cue, wave));
                }
            }
        }
    }
    let menu = disc
        .read("sound/acv2_bgm_menu_bgm_menu.fev")
        .and_then(|b| fev::read(&b))
        .ok()?;
    menu.waves("grg")
        .into_iter()
        .next()
        .cloned()
        .map(|w| ("grg".to_string(), w))
}

impl BgmState {
    pub fn is_muted(&self) -> bool {
        self.muted
    }

    fn spawn_decode(disc: &Disc, map: Option<&str>) -> Self {
        let Some((cue, wave)) = resolve_bgm_wave(disc, map) else {
            return Self {
                rx: None,
                muted: false,
                enabled: true,
            };
        };
        let disc = disc.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let file = format!("sound/{}.fsb", wave.bank);
            let sample = match fsb::read_sample(&disc, &file, wave.index as usize) {
                Ok(s) => s,
                Err(err) => {
                    eprintln!("sound: bgm {cue} ({file}#{}): {err:#}", wave.index);
                    return;
                }
            };
            let pcm = match sample.decode() {
                Ok(p) => p,
                Err(err) => {
                    eprintln!("sound: bgm {cue} decode: {err:#}");
                    return;
                }
            };
            let wav = wav_pcm(&pcm.to_i16(), pcm.channels, sample.frequency);
            let _ = tx.send(wav);
        });
        Self {
            rx: Some(Mutex::new(rx)),
            muted: false,
            enabled: true,
        }
    }
}

fn bgm_update(
    mut commands: Commands,
    mut assets: ResMut<Assets<AudioSource>>,
    mut bgm: ResMut<BgmState>,
    keys: Res<ButtonInput<KeyCode>>,
    global: Res<GlobalVolume>,
    mut sinks: Query<&mut AudioSink, With<BgmPlayer>>,
) {
    let master = global.volume.to_linear();
    if let Some(rx) = bgm.rx.as_ref() {
        let received = rx.lock().ok().and_then(|r| r.try_recv().ok());
        if let Some(wav) = received {
            bgm.rx = None;
            let handle = assets.add(AudioSource {
                bytes: Arc::<[u8]>::from(wav),
            });
            // Sink creation multiplies by `GlobalVolume`, so keep the relative BGM level here.
            let volume = if bgm.muted {
                Volume::Linear(0.0)
            } else {
                Volume::Linear(BGM_VOLUME)
            };
            commands.spawn((
                AudioPlayer(handle),
                PlaybackSettings::LOOP.with_volume(volume),
                BgmPlayer,
            ));
        }
    }
    if keys.just_pressed(KeyCode::F9) {
        bgm.muted = !bgm.muted;
        let vol = bgm_playback_volume(bgm.muted, master);
        for mut sink in &mut sinks {
            sink.set_volume(vol);
        }
    }
}

#[derive(Default)]
struct Heard {
    moving: bool,
    airborne: bool,
    glide: bool,
    quick_boosts: u32,
    wall_jumps: u32,
    kicks: u32,
    sustain: Vec<Entity>,
}

/// Boost start, boost sustain, high boost, glide start, jump, and animation TAE sound/footstep
/// events (360 `0x8289acd0`). Runs after the pilot and pose steps.
fn motion(
    mut commands: Commands,
    mut cues: ResMut<Cues>,
    mut assets: ResMut<Assets<AudioSource>>,
    piloting: Res<Piloting>,
    pilots: Query<&Pilot>,
    anim: Option<Res<crate::pose::Motion>>,
    mut heard: Local<Heard>,
) {
    let Ok(pilot) = pilots.single() else { return };
    if !piloting.0 {
        stop(&mut commands, &mut heard.sustain);
        heard.moving = false;
        heard.airborne = pilot.airborne;
        (heard.glide, heard.quick_boosts, heard.wall_jumps, heard.kicks) = (
            pilot.glide,
            pilot.quick_boosts,
            pilot.wall_jumps,
            pilot.kicks_started,
        );
        return;
    }
    if pilot.quick_boosts != heard.quick_boosts || pilot.kicks_started != heard.kicks {
        for cue in QUICK_BOOST {
            cues.play(&mut commands, &mut assets, cue, false);
        }
    }
    let wall_jumped = pilot.wall_jumps != heard.wall_jumps;
    if pilot.glide && !heard.glide {
        cues.play(&mut commands, &mut assets, GLIDE_ON, false);
    }
    (heard.glide, heard.quick_boosts, heard.wall_jumps, heard.kicks) = (
        pilot.glide,
        pilot.quick_boosts,
        pilot.wall_jumps,
        pilot.kicks_started,
    );
    let flat = Vec2::new(pilot.velocity.x, pilot.velocity.z);
    let moving = (pilot.boost || pilot.in_kick() || pilot.in_blade_dash()) && flat.length() > BOOST_MOVING;
    if moving && !heard.moving {
        cues.play(&mut commands, &mut assets, BOOST_ON, false);
    }
    if moving && heard.sustain.is_empty() {
        heard.sustain = cues.play(&mut commands, &mut assets, BOOST_LOOP, true);
    } else if !moving {
        stop(&mut commands, &mut heard.sustain);
    }
    heard.moving = moving;

    // Rising edge of airborne with upward speed: a jump. A ledge fall leaves `vy` ≤ 0, and a
    // grounded High Boost / Kick / Blade lunge adds its own vertical lift.
    // `se_jump` is +0xA of `AC_SOUNDPARAM_ST` (`FUN_82899168`, category 1 `c%08d`). Row 0
    // stands in for the AC's own row; every row stores the same id.
    let jump = wall_jumped
        || (pilot.airborne
            && !heard.airborne
            && pilot.velocity.y > 0.0
            && !pilot.in_quick_boost()
            && !pilot.in_kick()
            && !pilot.in_blade());
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

    // Animation TAE sound & footstep cues (360 `0x8289acd0`: kind 103 footstep, kinds 1/2/602 sound).
    if let Some(anim) = anim.as_deref() {
        for ev in &anim.tae_events {
            match ev {
                crate::pose::TaeFired::Footstep { .. } => {
                    let base = if anim.sound_param_id == 0 {
                        100
                    } else {
                        anim.sound_param_id
                    };
                    let cue = format!("c{:08}", base + 33);
                    if cues.play(&mut commands, &mut assets, &cue, false).is_empty() {
                        cues.play(&mut commands, &mut assets, "c00000133", false);
                    }
                }
                crate::pose::TaeFired::Sound { cue } => {
                    cues.play(&mut commands, &mut assets, cue, false);
                }
                crate::pose::TaeFired::Ffx { .. } => {}
            }
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
        assert_eq!(cues.projects.len(), 7);
        for cue in [
            "w00000034",
            "w00001012",
            "w00009027",
            "w00009224",
            "w00009600",
            "c00000089",
            BOOST_ON,
            BOOST_LOOP,
            "c00000024",
            QUICK_BOOST[0],
            QUICK_BOOST[1],
            GLIDE_ON,
            "menu_0000",
            "menu_0001",
        ] {
            let waves: Vec<fev::Wave> = cues
                .projects
                .iter()
                .find_map(|p| {
                    p.event(cue)
                        .map(|_| p.waves(cue).into_iter().cloned().collect())
                })
                .unwrap();
            let wave = &waves[0];
            let sample = cues.bank(&wave.bank).unwrap().samples[wave.index as usize].clone();
            assert!(
                sample.xma(),
                "{cue}: {} mode {:#x}",
                sample.name,
                sample.mode
            );
            let pcm = sample.decode().unwrap();
            assert_eq!(pcm.errors, 0, "{cue}");
            assert_eq!(
                pcm.samples.len(),
                sample.length as usize * sample.channels as usize
            );
            assert!(
                pcm.to_i16().iter().any(|&s| s.unsigned_abs() > 1000),
                "{cue} decoded to silence"
            );
        }
    }

    #[test]
    fn bgm_resolves_and_range_reads_x360() {
        let iso = acvd_formats::vfs::repo_root().join(acvd_formats::vfs::X360_ISO);
        let Ok(disc) = Disc::open(&iso) else { return };
        let (cue, wave) = resolve_bgm_wave(&disc, Some("m4000")).expect("m4000 bgm");
        assert_eq!(wave.bank, "bgm", "m4000 cue {cue}");
        let sample = fsb::read_sample(&disc, "sound/bgm.fsb", wave.index as usize).unwrap();
        assert!(sample.xma());
        assert_eq!(sample.channels, 2);
        assert!(sample.length > 44_100 * 30, "expected full-length BGM track");

        let (garage_cue, garage_wave) = resolve_bgm_wave(&disc, None).expect("garage bgm");
        assert_eq!(garage_cue, "grg");
        assert_eq!(garage_wave.bank, "bgm_menu");
        let gsample =
            fsb::read_sample(&disc, "sound/bgm_menu.fsb", garage_wave.index as usize).unwrap();
        assert!(gsample.xma());
        assert_eq!(gsample.channels, 2);
    }
}
