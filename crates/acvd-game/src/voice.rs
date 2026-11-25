//! COM / operator voice packs: the selected CPU voice that calls out AP, energy, scan/combat
//! mode, and combat events. The retail save key is `VoiceNo` (`0x82002418`), the selectable
//! on-disc packs are `acv2_voice_com_00..04.fev` (Type 1..5) with `0` = OFF, and the spoken line
//! for each `AI_MESSAGE_SOUND_MAP` row is the FEV event `%08d` reached through
//! `Sound_formatCue` (`0x82b47758`). See `sheets/voice.csv` for every address and message id.

use bevy::audio::AudioSource;
use bevy::prelude::*;

use crate::control::{Pilot, Piloting};
use crate::damage::PlayerHealth;
use crate::hud::Status;
use crate::settings::GameSettings;
use crate::sound::Cues;

/// `AI_MESSAGE_SOUND_MAP` row ids used here (text ids in `lang/en/text/ai/aisys.fmg`).
pub mod msg {
    /// `9018` AP at 50%.
    pub const AP_50: u32 = 1;
    /// `9019` AP at 30%.
    pub const AP_30: u32 = 2;
    /// `9020` AC disabled.
    pub const DESTROYED: u32 = 3;
    /// `9014` Energy at 30%.
    pub const ENERGY_30: u32 = 22;
    /// `9015` Energy depleted.
    pub const ENERGY_EMPTY: u32 = 23;
    /// `9016` System: Scan Mode.
    pub const SCAN_MODE: u32 = 26;
    /// `9017` System: Combat Mode.
    pub const COMBAT_MODE: u32 = 27;
    /// `9031` Beginning operation.
    pub const OP_START: u32 = 19;
    /// `9001` Target sighted. Engaging.
    pub const PREVIEW: u32 = 24;
}

/// Display label for a COM voice index (`0` = OFF, `1..=5` = Type 1..5).
pub fn com_voice_label(index: u8) -> &'static str {
    match index {
        0 => "OFF",
        1 => "TYPE 1",
        2 => "TYPE 2",
        3 => "TYPE 3",
        4 => "TYPE 4",
        5 => "TYPE 5",
        _ => "OFF",
    }
}

/// Resolves an `AI_MESSAGE_SOUND_MAP` voice offset to the FEV event id that
/// `Sound_formatCue` (`0x82b47758`, format 8 `%08d`) formats. The five on-disc packs all contain
/// the same event ids: `10001..10037`, then `10101`, `10201`, `10202`, `10203`, `20101`,
/// `20102`, `20103` (the offsets 38..44 the message table uses). `sheets/voice.csv`.
pub fn event_id(offset: u32) -> Option<u32> {
    Some(match offset {
        1..=37 => 10_000 + offset,
        38 => 10_101,
        39 => 10_201,
        40 => 10_202,
        41 => 10_203,
        42 => 20_101,
        43 => 20_102,
        44 => 20_103,
        _ => return None,
    })
}

/// Per-sortie latches so each threshold announcement plays once, plus a gap so lines do not
/// overlap (the 360 voice object sequences its lines).
#[derive(Resource, Default)]
pub struct ComVoiceState {
    started: bool,
    scan_mode: bool,
    ap_50: bool,
    ap_30: bool,
    destroyed: bool,
    energy_30: bool,
    energy_empty: bool,
    cooldown: f32,
}

/// Plays the selected COM voice for the standard in-combat callouts.
pub fn trigger_messages(
    mut commands: Commands,
    mut cues: ResMut<Cues>,
    mut assets: ResMut<Assets<AudioSource>>,
    settings: Res<GameSettings>,
    piloting: Res<Piloting>,
    time: Res<Time>,
    mut state: ResMut<ComVoiceState>,
    players: Query<(&Pilot, &Status, &PlayerHealth)>,
) {
    if !piloting.0 {
        *state = ComVoiceState::default();
        return;
    }
    let Ok((pilot, status, health)) = players.single() else {
        return;
    };
    state.cooldown = (state.cooldown - time.delta_secs()).max(0.0);

    let energy = status.energy_fraction();
    let ap = if health.ap_max > 0.0 {
        (health.ap / health.ap_max).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let mut want: Vec<u32> = Vec::new();

    if !state.started {
        state.started = true;
        want.push(msg::OP_START);
    }

    if pilot.scan_mode != state.scan_mode {
        state.scan_mode = pilot.scan_mode;
        want.push(if pilot.scan_mode {
            msg::SCAN_MODE
        } else {
            msg::COMBAT_MODE
        });
    }

    // Energy: warn once on the way down through 30% and again when fully depleted; re-arm when
    // the generator recharges.
    if energy > 0.35 {
        state.energy_30 = false;
    }
    if energy > 0.02 {
        state.energy_empty = false;
    }
    if energy < 0.30 && !state.energy_30 {
        state.energy_30 = true;
        want.push(msg::ENERGY_30);
    }
    if energy <= 0.001 && !state.energy_empty {
        state.energy_empty = true;
        want.push(msg::ENERGY_EMPTY);
    }

    // AP only falls (until AC TEST respawns the player, which refills it).
    if ap > 0.95 {
        state.ap_50 = false;
        state.ap_30 = false;
    }
    if ap < 0.50 && ap > 0.0 && !state.ap_50 {
        state.ap_50 = true;
        want.push(msg::AP_50);
    }
    if ap < 0.30 && ap > 0.0 && !state.ap_30 {
        state.ap_30 = true;
        want.push(msg::AP_30);
    }
    if health.destroyed && !state.destroyed {
        state.destroyed = true;
        want.push(msg::DESTROYED);
    } else if !health.destroyed {
        state.destroyed = false;
    }

    let voice = settings.com_voice();
    for offset in want {
        if voice == 0 || state.cooldown > 0.0 {
            break;
        }
        if !cues
            .com_voice(&mut commands, &mut assets, voice, offset)
            .is_empty()
        {
            state.cooldown = 2.0;
        }
    }
}

pub struct ComVoicePlugin;

impl Plugin for ComVoicePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ComVoiceState>()
            .add_systems(Update, trigger_messages);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_offsets_resolve_to_fev_event_ids() {
        assert_eq!(event_id(1), Some(10_001));
        assert_eq!(event_id(22), Some(10_022));
        assert_eq!(event_id(37), Some(10_037));
        assert_eq!(event_id(38), Some(10_101));
        assert_eq!(event_id(39), Some(10_201));
        assert_eq!(event_id(44), Some(20_103));
        assert_eq!(event_id(0), None);
        assert_eq!(event_id(45), None);
    }

    #[test]
    fn labels_cover_the_selectable_range() {
        assert_eq!(com_voice_label(0), "OFF");
        assert_eq!(com_voice_label(1), "TYPE 1");
        assert_eq!(com_voice_label(5), "TYPE 5");
        assert_eq!(com_voice_label(9), "OFF");
    }
}
