//! The first free-play cues. The 360 build formats a name (`Sound_formatCue` would be
//! `FUN_82b47758`; the format table is at `0x8371c688`) and plays it through MagicOrchestra.
//! This plays the sample that name indexes (XMA or 16-bit PCM, `acvd_formats::fsb::Sample::decode`).
//! Cue rows are `sheets/sound_cues.csv`.
//!
//! Playback is not positional. `FUN_82b46df0` takes a position, and the follow camera sits
//! far enough out that Bevy's spatial rolloff would silence the source. The speed crossfade
//! on `b00000003` is applied: its layer volume envelopes follow the FEV `speed` parameter the
//! 360 `AcSfxCtrl` update (`0x8289a138`) drives (see `update_sfx_mix`).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use acvd_data::generated::sound::PARAM_ACSOUNDPARAM_BIN;
use acvd_formats::vfs::Disc;
use acvd_formats::{env, fev, fsb};
use bevy::asset::Asset;
use bevy::audio::{
    AddAudioSource, AudioPlayer, AudioSink, AudioSinkPlayback, AudioSource, ChannelCount,
    Decodable, GlobalVolume, PlaybackSettings, SampleRate, Source, Volume,
};
use bevy::prelude::*;
use bevy::reflect::TypePath;

use crate::control::{Pilot, Piloting};

/// Same gate as the main-booster VFX (`sfx::boosters`): boost mode and moving.
const BOOST_MOVING: f32 = 0.05;
/// `AcSfxCtrl` slot 2 (`0x82098cb0`, `FUN_82899280`) plays this on the emitter at object+0x124.
const BOOST_ON: &str = "b00000000";
/// Boost sustain: `AcSfxCtrl`'s update (`0x8289a138`, vtable `0x82098c88` slot 0; the caller
/// reaches it from the AC update, LR `0x82893e20`) sends the FMOD parameter `"speed"`
/// (`0x82054a3c`) to the event instances at this+0x124/this+0x138 and starts/stops this cue
/// through `0x82899da8` (`b%08d`, id 3) while `cur*216` is above 0.001. It is the only booster
/// event with a `speed` parameter: two `main_boost11`/`main_boost12` sounds in layer 0 plus a
/// `main_boost03_hicut` layer, automated by the authored speed envelopes. The old single-layer
/// `b00000010` (`main_boost11`, -18 dB) is not it.
const BOOST_SUSTAIN: &str = "b00000003";
/// Speed (m/tick) where the FEV `speed` axis reaches 1.0. The writer `0x82892f08`
/// (LR `0x82892fa4`) sets `AcSfxCtrl+0x11c` to `|velocity| / 60` from the AC velocity
/// provider (owner+0x738 slot 0x2c); the update `0x8289a138` lerps `cur` 10% per 60 Hz tick
/// toward it and sends `cur * 216.0000153` (`0x8200D0A0`, km/h) on a 150-max FEV parameter.
/// So the envelope axis is `cur * 216 / 150 = cur * 1.44` and full scale is 150 km/h =
/// `150/216` m/tick. Probed boost target 0.26299 m/tick -> axis 0.3787, matching the retail
/// capture's 0.377.
const SPEED_AXIS_FULL_TICK: f32 = 150.0 / 216.0;
/// `AcSfxCtrl` slot +0x40 plays both (`boost_on06` + `boost11`, and `quick_boost03`): high boost.
const QUICK_BOOST: [&str; 2] = ["b00000007", "b00000008"];
/// `AcSfxCtrl` slots +0x30 / +0x3c (`dash00` + `boost_on06`), taken as the glide start; not traced
/// to the glide caller.
const GLIDE_ON: &str = "b00000006";

/// Audio channel bus attached to spawned cue entities so runtime volume sliders
/// can re-scale already-playing SFX and UI sounds independently of BGM.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioBus {
    Sfx,
    Ui,
}

/// FMOD Ex 4.40.05 ParamEQ parameter descriptions (`0x8374de50`, registered by `0x82f91d00`;
/// row `dsp.parameq` in `sheets/sound_cues.csv`): center frequency 20..22000 Hz, octave range
/// 0.2..5.0 and linear frequency gain 0.05..3.0 (1.0 = unity, so the peak is `20 log10(gain)`).
const PARAMEQ_CENTER: (f32, f32) = (20.0, 22000.0);
const PARAMEQ_CENTER_DEFAULT: f32 = 8000.0;
const PARAMEQ_OCTAVES: (f32, f32) = (0.2, 5.0);
const PARAMEQ_OCTAVES_DEFAULT: f32 = 1.0;
const PARAMEQ_GAIN: (f32, f32) = (0.05, 3.0);
const PARAMEQ_GAIN_DEFAULT: f32 = 1.0;

/// A looping voice whose gain follows the boost `speed` parameter (the FEV envelopes of
/// `b00000003`): base gain times every authored curve sampled at the normalized speed, times
/// the event's authored 3D distance gain (linear rolloff, `distance` = inner/outer metres).
#[derive(Component, Debug, Clone)]
struct SfxMix {
    base: f32,
    curves: Vec<Vec<(f32, f32)>>,
    /// The 360's smoothed `speed` target (`AcSfxCtrl+0x120`), in FEV axis units.
    smoothed: f32,
    distance: (f32, f32),
    /// `b00000003` layer 0's speed-automated ParamEQ, driven from the same axis.
    eq: Option<EqVoice>,
}

/// The authored ParamEQ curves plus the live filter state the audio thread reads.
#[derive(Debug, Clone)]
struct EqVoice {
    curves: fev::ParamEq,
    state: Arc<ParamEqState>,
}

/// Shared filter parameters (atomic f32 bits). `update_sfx_mix` writes one set per frame; the
/// custom [`BoostEqSource`] recomputes its biquad coefficients when they change.
#[derive(Debug)]
struct ParamEqState {
    center: AtomicU32,
    octaves: AtomicU32,
    gain: AtomicU32,
}

impl Default for ParamEqState {
    fn default() -> Self {
        Self {
            center: AtomicU32::new(PARAMEQ_CENTER_DEFAULT.to_bits()),
            octaves: AtomicU32::new(PARAMEQ_OCTAVES_DEFAULT.to_bits()),
            gain: AtomicU32::new(PARAMEQ_GAIN_DEFAULT.to_bits()),
        }
    }
}

impl ParamEqState {
    fn set(&self, center: f32, octaves: f32, gain: f32) {
        self.center.store(center.to_bits(), Ordering::Relaxed);
        self.octaves.store(octaves.to_bits(), Ordering::Relaxed);
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    fn get(&self) -> (f32, f32, f32) {
        (
            f32::from_bits(self.center.load(Ordering::Relaxed)),
            f32::from_bits(self.octaves.load(Ordering::Relaxed)),
            f32::from_bits(self.gain.load(Ordering::Relaxed)),
        )
    }
}

/// Decoded mono PCM of one booster layer voice plus the ParamEQ to apply while it plays.
#[derive(Asset, TypePath, Clone, Debug)]
struct BoostVoice {
    samples: Arc<[f32]>,
    rate: u32,
    eq: Arc<ParamEqState>,
}

impl Decodable for BoostVoice {
    type Decoder = BoostEqSource;

    fn decoder(&self) -> Self::Decoder {
        let params = self.eq.get();
        BoostEqSource {
            samples: self.samples.clone(),
            index: 0,
            rate: self.rate,
            eq: self.eq.clone(),
            filter: Biquad::peaking(self.rate as f32, params.0, params.1, params.2),
            applied: params,
            countdown: 0,
        }
    }
}

/// A finite rodio source over one loop of [`BoostVoice`]; `PlaybackSettings::LOOP` repeats it.
/// The biquad coefficients track the shared speed automation every 64 samples.
struct BoostEqSource {
    samples: Arc<[f32]>,
    index: usize,
    rate: u32,
    eq: Arc<ParamEqState>,
    filter: Biquad,
    applied: (f32, f32, f32),
    countdown: u32,
}

impl Iterator for BoostEqSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.index >= self.samples.len() {
            return None;
        }
        self.countdown += 1;
        if self.countdown >= 64 {
            self.countdown = 0;
            let params = self.eq.get();
            if (params.0 - self.applied.0).abs() > 1e-3
                || (params.1 - self.applied.1).abs() > 1e-4
                || (params.2 - self.applied.2).abs() > 1e-4
            {
                self.filter = Biquad::peaking(self.rate as f32, params.0, params.1, params.2);
                self.applied = params;
            }
        }
        let sample = self.samples[self.index];
        self.index += 1;
        Some(self.filter.process(sample))
    }
}

impl Source for BoostEqSource {
    fn current_span_len(&self) -> Option<usize> {
        Some(self.samples.len().saturating_sub(self.index))
    }

    fn channels(&self) -> ChannelCount {
        ChannelCount::new(1).unwrap()
    }

    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(self.rate.max(1)).unwrap()
    }

    fn total_duration(&self) -> Option<Duration> {
        Some(Duration::from_secs_f64(
            self.samples.len() as f64 / self.rate.max(1) as f64,
        ))
    }
}

/// RBJ peaking EQ biquad in transposed direct form II.
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// FMOD's ParamEQ: linear frequency gain (`gain`) and a width in octaves. The RBJ peaking
    /// response peaks at `20 log10(gain)` dB at `center_hz`.
    fn peaking(rate: f32, center_hz: f32, octaves: f32, gain: f32) -> Self {
        let a = gain.max(1e-4).sqrt();
        let w0 = 2.0 * std::f32::consts::PI * (center_hz / rate).clamp(1e-5, 0.49);
        let sin = w0.sin();
        let cos = w0.cos();
        let alpha = sin * ((2.0f32.ln() / 2.0) * octaves * w0 / sin.max(1e-6)).sinh();
        let a0 = 1.0 + alpha / a;
        Self {
            b0: (1.0 + alpha * a) / a0,
            b1: (-2.0 * cos) / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha / a) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

fn param_eq_value(curve: &[(f32, f32)], (min, max): (f32, f32), default: f32, x: f32) -> f32 {
    if curve.is_empty() {
        default
    } else {
        min + envelope_at(curve, x) * (max - min)
    }
}

/// Listener (follow camera) to player AC distance in metres, the input the FEV 3D rolloff
/// uses. `update_sfx_mix` refreshes it every frame; one-shots read it at spawn.
#[derive(Resource, Debug, Clone, Copy, Default)]
struct ListenerDistance(f32);

/// Piecewise-linear envelope sample, clamped to the first/last point.
fn envelope_at(points: &[(f32, f32)], x: f32) -> f32 {
    let Some(&(first_x, first_y)) = points.first() else {
        return 0.0;
    };
    if x <= first_x {
        return first_y;
    }
    for pair in points.windows(2) {
        let (x1, y1) = pair[1];
        if x <= x1 {
            let t = (x - pair[0].0) / (x1 - pair[0].0).max(1e-6);
            return pair[0].1 + (y1 - pair[0].1) * t;
        }
    }
    points.last().map(|p| p.1).unwrap_or(0.0)
}

/// Product of the voice's authored curves at `x`.
fn mix_gain(curves: &[Vec<(f32, f32)>], x: f32) -> f32 {
    curves
        .iter()
        .fold(1.0, |gain, points| gain * envelope_at(points, x))
}

/// Projects and the banks a cue has already pulled in.
#[derive(Resource)]
pub struct Cues {
    disc: Disc,
    projects: Vec<(fev::Project, AudioBus)>,
    /// Lazily loaded COM voice projects, keyed by pack index (`1..=5` → `acv2_voice_com_00..04`).
    voices: HashMap<u8, fev::Project>,
    banks: HashMap<String, fsb::Bank>,
    decoded: HashMap<(String, u32), Handle<AudioSource>>,
    /// f32 mono PCM (and rate) for the ParamEQ-filtered booster layers.
    pcm: HashMap<(String, u32), (Arc<[f32]>, u32)>,
    missing: HashSet<String>,
    sfx_volume: f32,
    ui_volume: f32,
}

impl Cues {
    fn load(disc: &Disc) -> Self {
        let mut projects = Vec::new();
        for (name, bus) in [
            ("acv2_se_weapon.fev", AudioBus::Sfx),
            ("acv2_se_bombs.fev", AudioBus::Sfx),
            ("acv2_se_booster.fev", AudioBus::Sfx),
            ("acv2_se_ac.fev", AudioBus::Sfx),
            ("acv2_se_enemy.fev", AudioBus::Sfx),
            ("acv2_se_menu.fev", AudioBus::Ui),
            ("acv2_se_garage.fev", AudioBus::Ui),
        ] {
            match disc
                .read(&format!("sound/{name}"))
                .and_then(|b| fev::read(&b))
            {
                Ok(project) => projects.push((project, bus)),
                Err(err) => eprintln!("sound: {err:#}"),
            }
        }
        Self {
            disc: disc.clone(),
            projects,
            voices: HashMap::new(),
            banks: HashMap::new(),
            decoded: HashMap::new(),
            pcm: HashMap::new(),
            missing: HashSet::new(),
            sfx_volume: 1.0,
            ui_volume: 1.0,
        }
    }

    pub fn set_bus_volumes(&mut self, sfx: f32, ui: f32) {
        self.sfx_volume = sfx.clamp(0.0, 1.0);
        self.ui_volume = ui.clamp(0.0, 1.0);
    }

    fn play(
        &mut self,
        commands: &mut Commands,
        assets: &mut Assets<AudioSource>,
        cue: &str,
        looping: bool,
    ) -> Vec<Entity> {
        self.play_at(commands, assets, cue, looping, None)
    }

    /// One-shot (or looping) cue with the authored 3D rolloff sampled at `distance` metres.
    /// Only pass a distance for events whose emitter is the player AC (booster cues); weapon
    /// and world cues have their own emitter positions and stay non-positional for now.
    fn play_at(
        &mut self,
        commands: &mut Commands,
        assets: &mut Assets<AudioSource>,
        cue: &str,
        looping: bool,
        distance: Option<f32>,
    ) -> Vec<Entity> {
        let found: Option<(Vec<fev::Wave>, f32, AudioBus)> =
            self.projects.iter().find_map(|(p, bus)| {
                p.event(cue).map(|e| {
                    let gain = distance.map_or(1.0, |d| e.distance_gain(d));
                    (
                        p.waves(cue).into_iter().cloned().collect(),
                        e.volume * gain,
                        *bus,
                    )
                })
            });
        let Some((waves, volume, bus)) = found else {
            if self.missing.insert(cue.to_string()) {
                eprintln!("sound: no cue {cue}");
            }
            return Vec::new();
        };
        if waves.is_empty() {
            if self.missing.insert(cue.to_string()) {
                eprintln!("sound: no cue {cue}");
            }
            return Vec::new();
        }
        self.play_waves(commands, assets, waves, bus, looping, volume)
    }

    /// Plays the boost sustain with its authored speed mix: every sound of every layer
    /// (`b00000003` has two `main_boost11`/`main_boost12` sounds plus the high-cut), at the
    /// FEV event volume and sound volume. Layer 0's first two volume envelopes are the
    /// per-sound speed gains and its next two are their speed fades; extra envelopes (the
    /// high-cut layer) are gains with no sound left to pair, so they are not applied.
    fn play_boost(
        &mut self,
        commands: &mut Commands,
        assets: &mut Assets<AudioSource>,
        boost_assets: &mut Assets<BoostVoice>,
        cue: &str,
    ) -> Vec<Entity> {
        let Some((project, bus)) = self.projects.iter().find(|(p, _)| p.event(cue).is_some())
        else {
            if self.missing.insert(cue.to_string()) {
                eprintln!("sound: no cue {cue}");
            }
            return Vec::new();
        };
        let bus = *bus;
        let Some(event) = project.event(cue) else {
            return Vec::new();
        };
        let bus_vol = match bus {
            AudioBus::Sfx => self.sfx_volume,
            AudioBus::Ui => self.ui_volume,
        };
        let distance = if event.is_3d() {
            (event.min_distance, event.max_distance)
        } else {
            (0.0, 0.0)
        };
        let eq = event.param_eq(0).map(|curves| EqVoice {
            curves,
            state: Arc::new(ParamEqState::default()),
        });
        let mut voices: Vec<(usize, fev::Wave, f32, Vec<Vec<(f32, f32)>>)> = Vec::new();
        for (li, layer) in event.mix.iter().enumerate() {
            let envs = event.volume_envelopes(li);
            let paired = envs.len() == layer.sounds.len() * 2;
            for (si, sound) in layer.sounds.iter().enumerate() {
                let Some(def) = project.defs.get(sound.def as usize) else {
                    continue;
                };
                let Some(wave) = def.wave.as_ref() else {
                    continue;
                };
                let mut curves = Vec::new();
                if let Some(gain) = envs.get(si) {
                    curves.push(gain.points.clone());
                }
                if paired {
                    if let Some(fade) = envs.get(layer.sounds.len() + si) {
                        curves.push(fade.points.clone());
                    }
                }
                let base = bus_vol * event.volume * sound.volume;
                voices.push((li, wave.clone(), base, curves));
            }
        }
        let initial = voices
            .iter()
            .map(|(_, _, base, curves)| base * mix_gain(curves, 0.0))
            .collect::<Vec<_>>();
        let mut spawned = Vec::with_capacity(voices.len());
        for ((li, wave, base, curves), initial) in voices.into_iter().zip(initial) {
            let eq = if li == 0 { eq.clone() } else { None };
            let mix = SfxMix {
                base,
                curves,
                smoothed: 0.0,
                distance,
                eq,
            };
            if let Some(voice) = &mix.eq {
                let Some(handle) = self.boost_source(boost_assets, &wave, voice.state.clone())
                else {
                    continue;
                };
                spawned.push(
                    commands
                        .spawn((
                            AudioPlayer(handle),
                            PlaybackSettings::LOOP.with_volume(Volume::Linear(initial)),
                            bus,
                            mix,
                        ))
                        .id(),
                );
                continue;
            }
            let Some(handle) = self.source(assets, &wave) else {
                continue;
            };
            spawned.push(
                commands
                    .spawn((
                        AudioPlayer(handle),
                        PlaybackSettings::LOOP.with_volume(Volume::Linear(initial)),
                        bus,
                        mix,
                    ))
                    .id(),
            );
        }
        spawned
    }

    fn play_waves(
        &mut self,
        commands: &mut Commands,
        assets: &mut Assets<AudioSource>,
        waves: Vec<fev::Wave>,
        bus: AudioBus,
        looping: bool,
        volume: f32,
    ) -> Vec<Entity> {
        let bus_vol = match bus {
            AudioBus::Sfx => self.sfx_volume,
            AudioBus::Ui => self.ui_volume,
        } * volume;
        let settings = if looping {
            PlaybackSettings::LOOP.with_volume(Volume::Linear(bus_vol))
        } else {
            PlaybackSettings::DESPAWN.with_volume(Volume::Linear(bus_vol))
        };
        let mut spawned = Vec::new();
        for wave in waves {
            let Some(handle) = self.source(assets, &wave) else {
                continue;
            };
            spawned.push(commands.spawn((AudioPlayer(handle), settings, bus)).id());
        }
        spawned
    }

    /// Plays one COM / operator voice line: `voice` is the pack index (`1..=5` → on-disc
    /// `acv2_voice_com_00..04.fev`) and `offset` is the `AI_MESSAGE_SOUND_MAP` voice offset
    /// (`sheets/voice.csv`). Returns the spawned audio entities.
    pub fn com_voice(
        &mut self,
        commands: &mut Commands,
        assets: &mut Assets<AudioSource>,
        voice: u8,
        offset: u32,
    ) -> Vec<Entity> {
        let Some(id) = crate::voice::event_id(offset) else {
            return Vec::new();
        };
        let cue = format!("{id:08}");
        let (waves, volume) = {
            let Some(project) = self.voice_project(voice) else {
                return Vec::new();
            };
            let waves = project.waves(&cue).into_iter().cloned().collect::<Vec<_>>();
            let volume = project.event(&cue).map(|e| e.volume).unwrap_or(1.0);
            (waves, volume)
        };
        self.play_waves(commands, assets, waves, AudioBus::Sfx, false, volume)
    }

    /// Length in seconds of one COM voice event, from the FSB sample count and frequency. The
    /// mission subtitle stays up for this long (`0x825196a8` writes its timer at window +0x170).
    pub fn com_voice_seconds(&mut self, voice: u8, offset: u32) -> Option<f32> {
        let id = crate::voice::event_id(offset)?;
        let cue = format!("{id:08}");
        let wave = {
            let project = self.voice_project(voice)?;
            project.waves(&cue).into_iter().next().cloned()?
        };
        let bank = self.bank(&wave.bank)?;
        let sample = bank.samples.get(wave.index as usize)?;
        (sample.frequency > 0).then(|| sample.length as f32 / sample.frequency as f32)
    }

    /// Plays a user-supplied COM voice line from disk (`.mp3`/`.wav`/`.ogg`/`.flac`, decoded by
    /// Bevy's audio loaders). Missing files log once and stay silent; the line rides the SFX bus.
    pub fn custom_voice(
        &mut self,
        commands: &mut Commands,
        assets: &mut Assets<AudioSource>,
        path: &str,
    ) -> Vec<Entity> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) => {
                if self.missing.insert(path.to_string()) {
                    eprintln!("sound: custom voice {path}: {err}");
                }
                return Vec::new();
            }
        };
        let handle = assets.add(AudioSource {
            bytes: bytes.into(),
        });
        let settings = PlaybackSettings::DESPAWN.with_volume(Volume::Linear(self.sfx_volume));
        vec![commands
            .spawn((AudioPlayer(handle), settings, AudioBus::Sfx))
            .id()]
    }

    fn voice_project(&mut self, voice: u8) -> Option<&fev::Project> {
        if voice == 0 || voice > crate::settings::COM_VOICE_MAX {
            return None;
        }
        if !self.voices.contains_key(&voice) {
            let name = format!("acv2_voice_com_{:02}.fev", voice - 1);
            match self
                .disc
                .read(&format!("sound/{name}"))
                .and_then(|b| fev::read(&b))
            {
                Ok(project) => {
                    self.voices.insert(voice, project);
                }
                Err(err) => {
                    if self.missing.insert(name.clone()) {
                        eprintln!("sound: {name}: {err:#}");
                    }
                    return None;
                }
            }
        }
        self.voices.get(&voice)
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

    /// Decoded f32 copy of a wave for the ParamEQ-filtered sustain layers.
    fn boost_source(
        &mut self,
        assets: &mut Assets<BoostVoice>,
        wave: &fev::Wave,
        eq: Arc<ParamEqState>,
    ) -> Option<Handle<BoostVoice>> {
        let key = (wave.bank.clone(), wave.index);
        if !self.pcm.contains_key(&key) {
            let decoded = self
                .bank(&wave.bank)
                .and_then(|b| b.samples.get(wave.index as usize))
                .map(|sample| (sample.decode(), sample.frequency));
            match decoded {
                Some((Ok(pcm), rate)) => {
                    self.pcm.insert(key.clone(), (Arc::from(pcm.samples), rate));
                }
                Some((Err(err), _)) => {
                    eprintln!("sound: {}#{}: {err:#}", wave.bank, wave.index);
                    return None;
                }
                None => {
                    let name = format!("{}#{}", wave.bank, wave.index);
                    if self.missing.insert(name.clone()) {
                        eprintln!("sound: {name} is not in the bank");
                    }
                    return None;
                }
            }
        }
        let (samples, rate) = self.pcm.get(&key)?.clone();
        Some(assets.add(BoostVoice { samples, rate, eq }))
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
            .init_resource::<ListenerDistance>()
            .add_audio_source::<BoostVoice>()
            .add_systems(Startup, sync_initial_bus_volumes)
            .add_systems(
                Update,
                (
                    motion.after(crate::weapons::fire),
                    bgm_update,
                    update_sfx_mix,
                ),
            );
    }
}

fn sync_initial_bus_volumes(
    settings: Option<Res<crate::settings::GameSettings>>,
    mut cues: ResMut<Cues>,
) {
    if let Some(s) = settings {
        cues.set_bus_volumes(s.sfx_volume, s.ui_volume);
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

    /// Enables BGM after a silent front end (`SoundPlugin { play_bgm: false }`); the next
    /// [`switch_bgm`] then spawns the decode thread.
    pub fn enable(&mut self) {
        self.enabled = true;
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
    settings: Option<Res<crate::settings::GameSettings>>,
    mut sinks: Query<&mut AudioSink, With<BgmPlayer>>,
) {
    let master = global.volume.to_linear();
    let music = settings
        .as_ref()
        .map(|s| s.music_volume.clamp(0.0, 1.0))
        .unwrap_or(1.0);
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
                Volume::Linear(BGM_VOLUME * music)
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
        let vol = bgm_playback_volume(bgm.muted, master * music);
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
    mut boost_assets: ResMut<Assets<BoostVoice>>,
    piloting: Res<Piloting>,
    listener: Res<ListenerDistance>,
    pilots: Query<&Pilot>,
    anim: Option<Res<crate::pose::Motion>>,
    mut heard: Local<Heard>,
) {
    let Ok(pilot) = pilots.single() else { return };
    if !piloting.0 {
        stop(&mut commands, &mut heard.sustain);
        heard.moving = false;
        heard.airborne = pilot.airborne;
        (
            heard.glide,
            heard.quick_boosts,
            heard.wall_jumps,
            heard.kicks,
        ) = (
            pilot.glide,
            pilot.quick_boosts,
            pilot.wall_jumps,
            pilot.kicks_started,
        );
        return;
    }
    if pilot.quick_boosts != heard.quick_boosts || pilot.kicks_started != heard.kicks {
        for cue in QUICK_BOOST {
            cues.play_at(&mut commands, &mut assets, cue, false, Some(listener.0));
        }
    }
    let wall_jumped = pilot.wall_jumps != heard.wall_jumps;
    if pilot.glide && !heard.glide {
        cues.play_at(
            &mut commands,
            &mut assets,
            GLIDE_ON,
            false,
            Some(listener.0),
        );
    }
    (
        heard.glide,
        heard.quick_boosts,
        heard.wall_jumps,
        heard.kicks,
    ) = (
        pilot.glide,
        pilot.quick_boosts,
        pilot.wall_jumps,
        pilot.kicks_started,
    );
    let flat = Vec2::new(pilot.velocity.x, pilot.velocity.z);
    let moving =
        (pilot.boost || pilot.in_kick() || pilot.in_blade_dash()) && flat.length() > BOOST_MOVING;
    if moving && !heard.moving {
        cues.play_at(
            &mut commands,
            &mut assets,
            BOOST_ON,
            false,
            Some(listener.0),
        );
    }
    if moving && heard.sustain.is_empty() {
        heard.sustain =
            cues.play_boost(&mut commands, &mut assets, &mut boost_assets, BOOST_SUSTAIN);
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
                    if cues
                        .play(&mut commands, &mut assets, &cue, false)
                        .is_empty()
                    {
                        cues.play(&mut commands, &mut assets, "c00000133", false);
                    }
                }
                crate::pose::TaeFired::Sound { cue } => {
                    cues.play(&mut commands, &mut assets, cue, false);
                }
                crate::pose::TaeFired::Ffx { .. }
                | crate::pose::TaeFired::Reattach { .. }
                | crate::pose::TaeFired::Purge { .. } => {}
            }
        }
    }
}

/// Advances the boost sustain mix every frame. The 360 `AcSfxCtrl` update (`0x8289a138`)
/// lerps its speed target by 10% per 60 Hz tick; the target is written by `0x82892f08`
/// (`AcSfxCtrl+0x11c` = `|velocity| / 60`, m/tick) and the update sends `target * 216`
/// (km/h) on the FEV `speed` parameter, whose max is 150, so the envelope axis is
/// `target * 1.44` (`SPEED_AXIS_FULL_TICK`). Each voice's gain is its base
/// gain times the authored envelopes sampled there, times the event's 3D distance gain: the
/// FEV `b00000003` is a 3D event with a 0..50 m linear rolloff and the follow camera sits
/// 16-21 m out (`sheets/camera_follow.csv`), so the sustain is 0.58-0.68 of its authored
/// level (the retail capture in `private/xenia/retail_audio.raw` measures ~0.6).
fn update_sfx_mix(
    time: Res<Time>,
    pilots: Query<(&Pilot, &Transform)>,
    cameras: Query<&Transform, (With<Camera3d>, Without<crate::env::WaterReflectionCamera>)>,
    mut listener_distance: ResMut<ListenerDistance>,
    mut sinks: Query<(&mut AudioSink, &mut SfxMix)>,
) {
    let Ok((pilot, pilot_transform)) = pilots.single() else {
        return;
    };
    let listener = cameras
        .iter()
        .next()
        .map(|camera| camera.translation)
        .unwrap_or(pilot_transform.translation);
    let distance = listener.distance(pilot_transform.translation);
    listener_distance.0 = distance;
    let target = (pilot.velocity.length() / SPEED_AXIS_FULL_TICK).clamp(0.0, 1.0);
    // `cur += 0.1 * (target - cur)` at 60 Hz in the 360, expressed for the frame delta.
    let alpha = 1.0 - 0.9f32.powf(60.0 * time.delta_secs());
    for (mut sink, mut mix) in &mut sinks {
        mix.smoothed += (target - mix.smoothed) * alpha;
        sink.set_volume(Volume::Linear(
            mix.base * mix_gain(&mix.curves, mix.smoothed) * falloff_gain(distance, mix.distance),
        ));
        if let Some(eq) = &mix.eq {
            let x = mix.smoothed;
            eq.state.set(
                param_eq_value(&eq.curves.center, PARAMEQ_CENTER, PARAMEQ_CENTER_DEFAULT, x),
                param_eq_value(
                    &eq.curves.octave_range,
                    PARAMEQ_OCTAVES,
                    PARAMEQ_OCTAVES_DEFAULT,
                    x,
                ),
                param_eq_value(&eq.curves.gain, PARAMEQ_GAIN, PARAMEQ_GAIN_DEFAULT, x),
            );
        }
    }
}

/// FMOD's linear 3D rolloff between the event's inner and outer distances. A non-3D voice
/// (or an event without a usable range) plays at full gain.
fn falloff_gain(distance: f32, (min_distance, max_distance): (f32, f32)) -> f32 {
    if max_distance <= min_distance {
        return 1.0;
    }
    ((max_distance - distance) / (max_distance - min_distance)).clamp(0.0, 1.0)
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

    /// `b00000003` layer 0: main_boost11 gain and fade-out, main_boost12 gain and fade-in.
    #[test]
    fn boost_speed_mix_uses_authored_envelopes() {
        let low_gain = [(0.0, 0.357699), (0.897727, 0.524306)];
        let low_fade = [(0.0, 1.0), (0.40884, 1.0), (1.0, 0.0)];
        let high_gain = [(0.0, 0.678201), (0.727273, 1.0)];
        let high_fade = [(0.0, 0.0), (0.095028, 0.0), (0.289503, 1.0)];
        let low = vec![low_gain.to_vec(), low_fade.to_vec()];
        let high = vec![high_gain.to_vec(), high_fade.to_vec()];
        assert!((mix_gain(&low, 0.0) - 0.357699).abs() < 1e-5);
        assert!(mix_gain(&high, 0.0).abs() < 1e-6);
        // Half speed: the high fade is complete while its gain still rises; the low fade has
        // just started.
        assert!((mix_gain(&high, 0.5) - 0.8994).abs() < 5e-3);
        assert!((mix_gain(&low, 0.5) - 0.3810).abs() < 5e-3);
        // Full speed: main_boost11 is faded out, main_boost12 at its top.
        assert!(mix_gain(&low, 1.0).abs() < 1e-6);
        assert!((mix_gain(&high, 1.0) - 1.0).abs() < 1e-5);
        // Clamped past the last point and empty curves are unity.
        assert!((envelope_at(&low_gain, 2.0) - 0.524306).abs() < 1e-6);
        assert_eq!(mix_gain(&[], 0.7), 1.0);
    }

    /// The FEV `speed` axis the 360 feeds `b00000003`: the writer `0x82892f08` (LR
    /// `0x82892fa4`) sets `AcSfxCtrl+0x11c` to `|velocity| / 60` (m/tick) and `0x8289a138`
    /// sends `cur * 216` (km/h) on a 150-max parameter, so the axis is `cur * 216 / 150`.
    /// The boost probe (`private/xenia/speed_int.jsonl`, setter `0x82899028`) measured
    /// 0.26299 m/tick -> 0.3787, the retail capture's 0.377.
    #[test]
    fn boost_speed_axis_matches_360_parameter() {
        let axis = |v_tick: f32| (v_tick / SPEED_AXIS_FULL_TICK).clamp(0.0, 1.0);
        assert!((axis(0.26299) - 0.3787).abs() < 1e-3);
        assert!((axis(0.555) - 0.7992).abs() < 2e-3);
        // Full scale is 150 km/h = 150/216 m/tick; FMOD clamps the normalized parameter.
        assert!((axis(150.0 / 216.0) - 1.0).abs() < 1e-6);
        assert_eq!(axis(400.0 / 216.0), 1.0);
    }

    /// `b00000003` is a 3D event: FEV mode `0x0408_0010`, linear rolloff 0..50 m. At the
    /// follow camera's 16-21 m the sustain is 0.58-0.68 of its authored level, matching the
    /// retail capture (`private/xenia/retail_audio.raw`) against the decoded FSB samples.
    #[test]
    fn boost_sustain_follows_authored_3d_rolloff() {
        let range = (0.0, 50.0);
        assert!((falloff_gain(0.0, range) - 1.0).abs() < 1e-6);
        assert!((falloff_gain(16.0, range) - 0.68).abs() < 1e-5);
        assert!((falloff_gain(20.0, range) - 0.60).abs() < 1e-5);
        assert_eq!(falloff_gain(50.0, range), 0.0);
        assert_eq!(falloff_gain(80.0, range), 0.0);
        // 2D voices (the quick-boost pair's `b00000007`) and events without a range are unity.
        assert_eq!(falloff_gain(500.0, (0.0, 0.0)), 1.0);
    }

    /// The booster one-shots carry their own authored 3D ranges: b00000000 0..100 m (boost
    /// start), b00000006 0..240 (glide), b00000008 0..200 (high boost), while b00000007 is 2D.
    /// `play_at` must fold `Event::distance_gain` into the spawn volume.
    #[test]
    fn boost_one_shots_apply_the_authored_3d_falloff() {
        let iso = acvd_formats::vfs::repo_root().join(acvd_formats::vfs::X360_ISO);
        let Ok(disc) = Disc::open(&iso) else { return };
        let mut app = App::new();
        app.insert_resource(Cues::load(&disc));
        app.insert_resource(Assets::<AudioSource>::default());
        app.add_systems(
            Update,
            |mut commands: Commands,
             mut cues: ResMut<Cues>,
             mut assets: ResMut<Assets<AudioSource>>| {
                for cue in [BOOST_ON, QUICK_BOOST[0], GLIDE_ON, QUICK_BOOST[1]] {
                    cues.play_at(&mut commands, &mut assets, cue, false, Some(20.0));
                }
            },
        );
        app.update();
        let world = app.world_mut();
        let mut query = world.query::<&PlaybackSettings>();
        let mut volumes: Vec<f32> = query
            .iter(world)
            .map(|settings| match settings.volume {
                Volume::Linear(v) => v,
                _ => panic!("unexpected volume curve"),
            })
            .collect();
        volumes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        // Sorted: boost_on06 at 0.8, quick_boost03 at 0.9, dash00 + boost_on06 at 1-20/240,
        // then the 2D high-boost partner's two layers at unity.
        let expected = [
            0.467735 * 0.8,
            0.501187 * 0.9,
            0.707946 * (1.0 - 20.0 / 240.0),
            0.707946 * (1.0 - 20.0 / 240.0),
            0.707946,
            0.707946,
        ];
        assert_eq!(volumes.len(), expected.len(), "{volumes:?}");
        for (volume, expected) in volumes.iter().zip(expected) {
            assert!(
                (volume - expected).abs() < 1e-4,
                "{volume} != {expected} in {volumes:?}"
            );
        }
    }

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
            BOOST_SUSTAIN,
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
                .find_map(|(p, _)| {
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
        assert!(
            sample.length > 44_100 * 30,
            "expected full-length BGM track"
        );

        let (garage_cue, garage_wave) = resolve_bgm_wave(&disc, None).expect("garage bgm");
        assert_eq!(garage_cue, "grg");
        assert_eq!(garage_wave.bank, "bgm_menu");
        let gsample =
            fsb::read_sample(&disc, "sound/bgm_menu.fsb", garage_wave.index as usize).unwrap();
        assert!(gsample.xma());
        assert_eq!(gsample.channels, 2);
    }

    /// The `FMOD ParamEQ` of `b00000003` layer 0, mapped from its normalized FEV curves onto
    /// the 360 DSP parameter ranges (`0x8374de50`): center 20..22000 Hz, 0.2..5.0 octaves,
    /// linear gain 0.05..3.0. At the retail-capture speed (axis 0.377 = 0.263 m/tick,
    /// 56.8 km/h) the filter is ~unity, which is why the captured RMS matched the dry mix;
    /// at full speed it is +2.4 dB.
    #[test]
    fn boost_param_eq_uses_the_360_dsp_ranges() {
        let iso = acvd_formats::vfs::repo_root().join(acvd_formats::vfs::X360_ISO);
        let Ok(disc) = Disc::open(&iso) else { return };
        let cues = Cues::load(&disc);
        let project = cues
            .projects
            .iter()
            .find_map(|(p, _)| p.event(BOOST_SUSTAIN).map(|_| p))
            .unwrap();
        let event = project.event(BOOST_SUSTAIN).unwrap();
        let eq = event.param_eq(0).expect("layer 0 ParamEQ");
        assert!(event.param_eq(1).is_none());
        let at = |x: f32| {
            (
                param_eq_value(&eq.center, PARAMEQ_CENTER, PARAMEQ_CENTER_DEFAULT, x),
                param_eq_value(
                    &eq.octave_range,
                    PARAMEQ_OCTAVES,
                    PARAMEQ_OCTAVES_DEFAULT,
                    x,
                ),
                param_eq_value(&eq.gain, PARAMEQ_GAIN, PARAMEQ_GAIN_DEFAULT, x),
            )
        };
        // Center 20 + 0.719723 * 21980, octave 0.2 + 0.329676 * 4.8.
        let (center, octaves, gain) = at(0.377);
        assert!((center - 15839.5).abs() < 5.0, "{center}");
        assert!((octaves - 1.7824).abs() < 1e-3, "{octaves}");
        assert!((gain - 1.007).abs() < 0.02, "{gain}");
        // At the speed-envelope top the gain curve reaches 0.430556 -> 1.3202 (+2.41 dB).
        let (center, _, gain) = at(1.0);
        assert!((center - 15839.5).abs() < 5.0, "{center}");
        assert!((gain - 1.3202).abs() < 1e-3, "{gain}");
    }

    /// Process a sine through the biquad and return its steady-state RMS gain.
    fn filter_gain(center: f32, octaves: f32, gain: f32, freq: f32) -> f32 {
        let rate = 44_100.0;
        let mut filter = Biquad::peaking(rate, center, octaves, gain);
        let mut sum_in = 0.0f64;
        let mut sum_out = 0.0f64;
        let n = 44_100;
        for i in 0..n {
            let t = i as f32 / rate;
            let x = (2.0 * std::f32::consts::PI * freq * t).sin();
            let y = filter.process(x);
            if i > n / 2 {
                sum_in += (x * x) as f64;
                sum_out += (y * y) as f64;
            }
        }
        (sum_out / sum_in).sqrt() as f32
    }

    #[test]
    fn param_eq_biquad_peaks_at_the_authored_gain() {
        // Unity gain is bit-exact identity (FMOD's default).
        assert!((filter_gain(8000.0, 1.0, 1.0, 1000.0) - 1.0).abs() < 1e-5);
        // A +6 dB linear gain of 2 peaks at the center and leaves low frequencies alone.
        assert!((filter_gain(1000.0, 1.0, 2.0, 1000.0) - 2.0).abs() < 0.02);
        assert!((filter_gain(1000.0, 1.0, 2.0, 100.0) - 1.0).abs() < 0.03);
        // The authored top-speed state: 15.8 kHz, 1.78 octaves, 1.32 (+2.41 dB).
        assert!((filter_gain(15839.5, 1.7824, 1.3202, 15839.5) - 1.3202).abs() < 0.02);
    }

    #[test]
    fn boost_eq_source_follows_the_shared_speed_state() {
        let rate = 44_100;
        let freq = 1000.0;
        let samples: Arc<[f32]> = (0..rate)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect::<Vec<f32>>()
            .into();
        let eq = Arc::new(ParamEqState::default());
        let voice = BoostVoice {
            samples,
            rate,
            eq: eq.clone(),
        };
        let mut source = voice.decoder();
        let mut sum = 0.0f32;
        let mut count = 0;
        for _ in 0..2048 {
            sum += source.next().unwrap().abs();
            count += 1;
        }
        let default_mean = sum / count as f32;
        // A flat pass-through before `update_sfx_mix` sets the authored state.
        assert!((default_mean - 0.6366).abs() < 0.01, "{default_mean}");
        eq.set(freq, 1.0, 2.0);
        let mut sum = 0.0f32;
        let mut count = 0;
        for _ in 0..8192 {
            sum += source.next().unwrap().abs();
            count += 1;
        }
        let boosted_mean = sum / count as f32;
        // The +6 dB state takes effect within the 64-sample refresh.
        assert!((boosted_mean - 2.0 * 0.6366).abs() < 0.05, "{boosted_mean}");
    }
}
