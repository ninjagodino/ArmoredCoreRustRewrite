//! Local player options (fullscreen + master volume), persisted as `settings.json`.

use bevy::audio::{AudioSink, AudioSinkPlayback, GlobalVolume, Volume};
use bevy::prelude::*;
use bevy::window::{MonitorSelection, WindowMode};
use serde::{Deserialize, Serialize};

use crate::sound::{self, BgmPlayer, BgmState};

pub const SETTINGS_PATH: &str = "settings.json";

/// Left/Right volume nudge in the garage Options menu (5% steps).
pub const VOLUME_STEP_PCT: i8 = 5;

#[derive(Resource, Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GameSettings {
    pub fullscreen: bool,
    /// Linear master volume in `0.0..=1.0` (shown as 0–100% in the garage Options menu).
    pub volume: f32,
}

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            fullscreen: false,
            volume: 1.0,
        }
    }
}

impl GameSettings {
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(SETTINGS_PATH) else {
            return Self::default();
        };
        let mut loaded: Self = serde_json::from_str(&text).unwrap_or_default();
        loaded.volume = loaded.volume.clamp(0.0, 1.0);
        loaded
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(SETTINGS_PATH, json);
        }
    }

    pub fn volume_percent(&self) -> u8 {
        (self.volume * 100.0).round().clamp(0.0, 100.0) as u8
    }

    pub fn step_volume(&mut self, delta_steps: i8) {
        let pct = self.volume_percent() as i16 + i16::from(delta_steps) * i16::from(VOLUME_STEP_PCT);
        self.volume = (pct.clamp(0, 100) as f32) / 100.0;
    }
}

pub fn apply_fullscreen(window: &mut Window, fullscreen: bool) {
    window.mode = if fullscreen {
        WindowMode::BorderlessFullscreen(MonitorSelection::Current)
    } else {
        WindowMode::Windowed
    };
}

/// Sets Bevy [`GlobalVolume`] and re-scales already-playing sinks (Bevy only applies
/// [`GlobalVolume`] when a sink is first created).
pub fn apply_volume<'w, 's>(
    settings: &GameSettings,
    bgm: &BgmState,
    global: &mut GlobalVolume,
    sinks: &mut Query<'w, 's, (Entity, &'static mut AudioSink)>,
    bgm_players: &Query<'w, 's, Entity, With<BgmPlayer>>,
) {
    let master = settings.volume.clamp(0.0, 1.0);
    global.volume = Volume::Linear(master);
    let bgm_vol = sound::bgm_playback_volume(bgm.is_muted(), master);
    for (entity, mut sink) in sinks.iter_mut() {
        let is_bgm = bgm_players.get(entity).is_ok();
        sink.set_volume(if is_bgm {
            bgm_vol
        } else {
            Volume::Linear(master)
        });
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
}
