//! Local player options (display, UI scaling, and audio mix), persisted as `settings.json`.

use bevy::audio::{AudioSink, AudioSinkPlayback, GlobalVolume, Volume};
use bevy::prelude::*;
use bevy::window::{MonitorSelection, WindowMode};
use serde::{Deserialize, Serialize};

use crate::sound::{self, AudioBus, BgmPlayer, BgmState, Cues};

pub const SETTINGS_PATH: &str = "settings.json";

/// Left/Right volume nudge in Options menus (5% steps).
pub const VOLUME_STEP_PCT: i8 = 5;

/// Left/Right UI scale nudge in Options menus (5% steps).
pub const UI_SCALE_STEP_PCT: i8 = 5;
/// Minimum UI scale multiplier (75%).
pub const UI_SCALE_MIN: f32 = 0.75;
/// Maximum UI scale multiplier (150%).
pub const UI_SCALE_MAX: f32 = 1.50;

/// Individual audio mixer sliders exposed in the Options menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioChannel {
    Master,
    Music,
    Sfx,
    Ui,
}

impl AudioChannel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Master => "MASTER VOLUME",
            Self::Music => "MUSIC VOLUME",
            Self::Sfx => "SFX VOLUME",
            Self::Ui => "MENU / UI VOLUME",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Master => "MASTER",
            Self::Music => "MUSIC",
            Self::Sfx => "SFX",
            Self::Ui => "MENU / UI",
        }
    }
}

#[derive(Resource, Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GameSettings {
    pub fullscreen: bool,
    /// UI & HUD scale multiplier in `0.75..=1.50` (75–150%, default 100%).
    pub ui_scale: f32,
    /// Crosshair / lock-sight HUD sway intensity in `0.0..=1.0` (0–100%, default 100%).
    pub hud_sway: f32,
    /// Combat HUD (lock-sight ring, digits, gauges) RGB color (default `#00FFA5`).
    pub hud_color: [u8; 3],
    /// Linear master volume in `0.0..=1.0` (0–100%).
    pub volume: f32,
    /// Linear music/BGM volume multiplier in `0.0..=1.0` (0–100%).
    pub music_volume: f32,
    /// Linear combat/world SFX volume multiplier in `0.0..=1.0` (0–100%).
    pub sfx_volume: f32,
    /// Linear menu/UI cue volume multiplier in `0.0..=1.0` (0–100%).
    pub ui_volume: f32,
}

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            fullscreen: false,
            ui_scale: 1.0,
            hud_sway: 1.0,
            hud_color: [0x00, 0xff, 0xa5],
            volume: 1.0,
            music_volume: 1.0,
            sfx_volume: 1.0,
            ui_volume: 1.0,
        }
    }
}

impl GameSettings {
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(SETTINGS_PATH) else {
            return Self::default();
        };
        let mut loaded: Self = serde_json::from_str(&text).unwrap_or_default();
        loaded.clamp_all();
        loaded
    }

    pub fn clamp_all(&mut self) {
        self.ui_scale = self.ui_scale.clamp(UI_SCALE_MIN, UI_SCALE_MAX);
        self.hud_sway = self.hud_sway.clamp(0.0, 1.0);
        self.volume = self.volume.clamp(0.0, 1.0);
        self.music_volume = self.music_volume.clamp(0.0, 1.0);
        self.sfx_volume = self.sfx_volume.clamp(0.0, 1.0);
        self.ui_volume = self.ui_volume.clamp(0.0, 1.0);
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(SETTINGS_PATH, json);
        }
    }

    pub fn ui_scale_percent(&self) -> u16 {
        (self.ui_scale * 100.0)
            .round()
            .clamp(UI_SCALE_MIN * 100.0, UI_SCALE_MAX * 100.0) as u16
    }

    /// Normalized `0.0..=1.0` gauge fill for `ui_scale` across `UI_SCALE_MIN..=UI_SCALE_MAX`.
    pub fn ui_scale_fill(&self) -> f32 {
        ((self.ui_scale - UI_SCALE_MIN) / (UI_SCALE_MAX - UI_SCALE_MIN)).clamp(0.0, 1.0)
    }

    /// Normalized position of the default `100%` UI scale marker on the gauge bar.
    pub fn ui_scale_default_mark() -> f32 {
        (1.0 - UI_SCALE_MIN) / (UI_SCALE_MAX - UI_SCALE_MIN)
    }

    pub fn step_ui_scale(&mut self, delta_steps: i8) {
        let min_pct = (UI_SCALE_MIN * 100.0).round() as i16;
        let max_pct = (UI_SCALE_MAX * 100.0).round() as i16;
        let pct = self.ui_scale_percent() as i16
            + i16::from(delta_steps) * i16::from(UI_SCALE_STEP_PCT);
        self.ui_scale = (pct.clamp(min_pct, max_pct) as f32) / 100.0;
    }

    pub fn set_ui_scale_from_fill(&mut self, fill: f32) {
        let t = fill.clamp(0.0, 1.0);
        let min_pct = (UI_SCALE_MIN * 100.0).round() as i16;
        let max_pct = (UI_SCALE_MAX * 100.0).round() as i16;
        let pct = ((UI_SCALE_MIN + t * (UI_SCALE_MAX - UI_SCALE_MIN)) * 100.0).round() as i16;
        self.ui_scale = (pct.clamp(min_pct, max_pct) as f32) / 100.0;
    }

    pub fn hud_sway_percent(&self) -> u8 {
        (self.hud_sway.clamp(0.0, 1.0) * 100.0).round() as u8
    }

    pub fn step_hud_sway(&mut self, delta_steps: i8) {
        let pct = self.hud_sway_percent() as i16
            + i16::from(delta_steps) * i16::from(VOLUME_STEP_PCT);
        self.hud_sway = (pct.clamp(0, 100) as f32) / 100.0;
    }

    pub fn set_hud_sway_from_fill(&mut self, fill: f32) {
        let pct = (fill.clamp(0.0, 1.0) * 100.0).round().clamp(0.0, 100.0) as u8;
        self.hud_sway = (pct as f32) / 100.0;
    }

    pub fn channel_volume(&self, channel: AudioChannel) -> f32 {
        match channel {
            AudioChannel::Master => self.volume,
            AudioChannel::Music => self.music_volume,
            AudioChannel::Sfx => self.sfx_volume,
            AudioChannel::Ui => self.ui_volume,
        }
        .clamp(0.0, 1.0)
    }

    pub fn channel_percent(&self, channel: AudioChannel) -> u8 {
        (self.channel_volume(channel) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8
    }

    pub fn step_channel(&mut self, channel: AudioChannel, delta_steps: i8) {
        let pct = self.channel_percent(channel) as i16
            + i16::from(delta_steps) * i16::from(VOLUME_STEP_PCT);
        let next = (pct.clamp(0, 100) as f32) / 100.0;
        match channel {
            AudioChannel::Master => self.volume = next,
            AudioChannel::Music => self.music_volume = next,
            AudioChannel::Sfx => self.sfx_volume = next,
            AudioChannel::Ui => self.ui_volume = next,
        }
    }

    pub fn set_channel_from_fill(&mut self, channel: AudioChannel, fill: f32) {
        let pct = (fill.clamp(0.0, 1.0) * 100.0).round().clamp(0.0, 100.0) as u8;
        let next = (pct as f32) / 100.0;
        match channel {
            AudioChannel::Master => self.volume = next,
            AudioChannel::Music => self.music_volume = next,
            AudioChannel::Sfx => self.sfx_volume = next,
            AudioChannel::Ui => self.ui_volume = next,
        }
    }

    #[cfg(test)]
    pub fn volume_percent(&self) -> u8 {
        self.channel_percent(AudioChannel::Master)
    }

    #[cfg(test)]
    pub fn step_volume(&mut self, delta_steps: i8) {
        self.step_channel(AudioChannel::Master, delta_steps);
    }
}

pub fn apply_fullscreen(window: &mut Window, fullscreen: bool) {
    window.mode = if fullscreen {
        WindowMode::BorderlessFullscreen(MonitorSelection::Current)
    } else {
        WindowMode::Windowed
    };
}

/// Sets Bevy [`GlobalVolume`], updates future cue bus gains on [`Cues`], and re-scales
/// already-playing sinks (Bevy only applies [`GlobalVolume`] when a sink is first created).
pub fn apply_volume<'w, 's>(
    settings: &GameSettings,
    bgm: &BgmState,
    cues: &mut Cues,
    global: &mut GlobalVolume,
    sinks: &mut Query<'w, 's, (Entity, &'static mut AudioSink)>,
    bgm_players: &Query<'w, 's, Entity, With<BgmPlayer>>,
    audio_buses: &Query<'w, 's, &'static AudioBus>,
) {
    let master = settings.volume.clamp(0.0, 1.0);
    let music = settings.music_volume.clamp(0.0, 1.0);
    let sfx = settings.sfx_volume.clamp(0.0, 1.0);
    let ui = settings.ui_volume.clamp(0.0, 1.0);

    global.volume = Volume::Linear(master);
    cues.set_bus_volumes(sfx, ui);

    let bgm_vol = sound::bgm_playback_volume(bgm.is_muted(), master * music);
    let sfx_vol = Volume::Linear(master * sfx);
    let ui_vol = Volume::Linear(master * ui);

    for (entity, mut sink) in sinks.iter_mut() {
        if bgm_players.get(entity).is_ok() {
            sink.set_volume(bgm_vol);
        } else if matches!(audio_buses.get(entity), Ok(AudioBus::Ui)) {
            sink.set_volume(ui_vol);
        } else {
            sink.set_volume(sfx_vol);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_steps_clamp_to_percent_range() {
        let mut s = GameSettings {
            volume: 0.02,
            ..Default::default()
        };
        s.step_volume(-1);
        assert_eq!(s.volume_percent(), 0);
        s.volume = 0.98;
        s.step_volume(1);
        assert_eq!(s.volume_percent(), 100);
        s.step_volume(1);
        assert_eq!(s.volume_percent(), 100);
    }

    #[test]
    fn audio_channels_and_ui_scale_step_and_clamp() {
        let mut s = GameSettings::default();
        assert_eq!(s.ui_scale_percent(), 100);
        assert!((s.ui_scale_fill() - GameSettings::ui_scale_default_mark()).abs() < 1e-4);

        for _ in 0..10 {
            s.step_ui_scale(-1);
        }
        assert_eq!(s.ui_scale_percent(), 75);
        assert_eq!(s.ui_scale_fill(), 0.0);

        for _ in 0..20 {
            s.step_ui_scale(1);
        }
        assert_eq!(s.ui_scale_percent(), 150);
        assert_eq!(s.ui_scale_fill(), 1.0);

        for ch in [
            AudioChannel::Master,
            AudioChannel::Music,
            AudioChannel::Sfx,
            AudioChannel::Ui,
        ] {
            s.step_channel(ch, -3);
            assert_eq!(s.channel_percent(ch), 85);
            for _ in 0..25 {
                s.step_channel(ch, -1);
            }
            assert_eq!(s.channel_percent(ch), 0);
            for _ in 0..25 {
                s.step_channel(ch, 1);
            }
            assert_eq!(s.channel_percent(ch), 100);
        }
    }

    #[test]
    fn legacy_settings_json_deserializes_with_defaults() {
        let legacy = r#"{"fullscreen": true, "volume": 0.65}"#;
        let mut parsed: GameSettings = serde_json::from_str(legacy).unwrap();
        parsed.clamp_all();
        assert!(parsed.fullscreen);
        assert_eq!(parsed.volume_percent(), 65);
        assert_eq!(parsed.ui_scale_percent(), 100);
        assert_eq!(parsed.channel_percent(AudioChannel::Music), 100);
        assert_eq!(parsed.channel_percent(AudioChannel::Sfx), 100);
        assert_eq!(parsed.channel_percent(AudioChannel::Ui), 100);
    }
}
