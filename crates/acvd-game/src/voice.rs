//! COM / operator voice packs: the selected CPU voice that calls out AP, energy, scan/combat
//! mode, and combat events. The retail save key is `VoiceNo` (`0x82002418`), the selectable
//! on-disc packs are `acv2_voice_com_00..04.fev` (Type 1..5) with `0` = OFF, and the spoken line
//! for each `AI_MESSAGE_SOUND_MAP` row is the FEV event `%08d` reached through
//! `Sound_formatCue` (`0x82b47758`). See `sheets/voice.csv` for every address and message id.

use std::collections::BTreeMap;

use bevy::audio::AudioSource;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::control::{Pilot, Piloting};
use crate::damage::PlayerHealth;
use crate::hud::Status;
use crate::settings::GameSettings;
use crate::sound::Cues;

/// Local (gitignored) manifest of user-made COM voice packs, in the working directory beside
/// `settings.json`. Rewrite-only feature; the retail 360 game only has the five on-disc packs.
pub const CUSTOM_VOICE_MANIFEST: &str = "custom_voices.json";
/// Maximum length of a user pack name shown in the Options slider.
pub const MAX_PACK_NAME: usize = 20;

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

/// One callout a custom pack can replace: the `AI_MESSAGE_SOUND_MAP` offset plus the editor label.
#[derive(Debug, Clone, Copy)]
pub struct ComEditorEvent {
    pub offset: u32,
    pub label: &'static str,
}

/// Callouts exposed in the custom COM voice editor, in display order. Offsets match
/// `sheets/voice.csv`; `PREVIEW` also backs the slider's sample line.
pub const EDITOR_EVENTS: &[ComEditorEvent] = &[
    ComEditorEvent {
        offset: msg::OP_START,
        label: "BEGINNING OPERATION",
    },
    ComEditorEvent {
        offset: msg::ENERGY_30,
        label: "ENERGY AT 30%",
    },
    ComEditorEvent {
        offset: msg::ENERGY_EMPTY,
        label: "ENERGY DEPLETED",
    },
    ComEditorEvent {
        offset: msg::AP_50,
        label: "AP AT 50%",
    },
    ComEditorEvent {
        offset: msg::AP_30,
        label: "AP AT 30%",
    },
    ComEditorEvent {
        offset: msg::DESTROYED,
        label: "AC DISABLED",
    },
    ComEditorEvent {
        offset: msg::SCAN_MODE,
        label: "SCAN MODE",
    },
    ComEditorEvent {
        offset: msg::COMBAT_MODE,
        label: "COMBAT MODE",
    },
    ComEditorEvent {
        offset: msg::PREVIEW,
        label: "TARGET SIGHTED (PREVIEW)",
    },
];

/// User-made COM voice pack: a display name plus one audio file per callout offset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustomVoicePack {
    pub name: String,
    /// `AI_MESSAGE_SOUND_MAP` offset -> absolute audio path (`.mp3`/`.wav`/`.ogg`/`.flac`).
    #[serde(default)]
    pub lines: BTreeMap<u32, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct VoiceManifest {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    packs: Vec<CustomVoicePack>,
}

/// Loaded custom COM voice packs (`custom_voices.json`). The Options slider walks
/// `OFF`, the five on-disc types, then these packs by saved name.
#[derive(Resource, Default)]
pub struct VoiceLibrary {
    pub packs: Vec<CustomVoicePack>,
}

impl VoiceLibrary {
    /// Reads `custom_voices.json`; a missing or malformed manifest yields an empty library.
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(CUSTOM_VOICE_MANIFEST) else {
            return Self::default();
        };
        match serde_json::from_str::<VoiceManifest>(&text) {
            Ok(manifest) => Self {
                packs: manifest.packs,
            },
            Err(err) => {
                eprintln!("voice: {CUSTOM_VOICE_MANIFEST}: {err}");
                Self::default()
            }
        }
    }

    /// Writes the manifest (pretty JSON) back to the working directory.
    pub fn save(&self) -> anyhow::Result<()> {
        let manifest = VoiceManifest {
            version: 1,
            packs: self.packs.clone(),
        };
        std::fs::write(
            CUSTOM_VOICE_MANIFEST,
            serde_json::to_string_pretty(&manifest)?,
        )?;
        Ok(())
    }

    /// Slider slot count: `OFF` + five on-disc types + one slot per custom pack.
    pub fn slot_count(&self) -> usize {
        6 + self.packs.len()
    }

    /// Active slider slot for `settings` (custom pack name wins when it still exists).
    pub fn selection(&self, settings: &GameSettings) -> usize {
        if let Some(name) = settings.com_voice_custom.as_deref() {
            if let Some(idx) = self.packs.iter().position(|p| p.name == name) {
                return 6 + idx;
            }
        }
        settings.com_voice().min(5) as usize
    }

    /// Display label for one slider slot (`OFF`, `TYPE 1..5`, or the custom pack name).
    pub fn label_at(&self, slot: usize) -> String {
        match slot {
            0 => "OFF".to_string(),
            1..=5 => format!("TYPE {slot}"),
            _ => self
                .packs
                .get(slot - 6)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "OFF".to_string()),
        }
    }

    /// Display label for the current selection.
    pub fn label(&self, settings: &GameSettings) -> String {
        self.label_at(self.selection(settings))
    }

    /// Normalized `0.0..=1.0` gauge fill across every selectable slot.
    pub fn fill(&self, settings: &GameSettings) -> f32 {
        let count = self.slot_count();
        if count <= 1 {
            return 0.0;
        }
        self.selection(settings) as f32 / (count - 1) as f32
    }

    /// Selects a slider slot, updating both the built-in index and the custom pack name.
    pub fn apply_slot(&self, settings: &mut GameSettings, slot: usize) {
        let slot = slot.min(self.slot_count().saturating_sub(1));
        if slot <= 5 {
            settings.com_voice = slot as u8;
            settings.com_voice_custom = None;
        } else if let Some(pack) = self.packs.get(slot - 6) {
            settings.com_voice_custom = Some(pack.name.clone());
        }
    }

    /// Cycles selection through `OFF` -> `TYPE 1..5` -> custom packs (wraps both ways).
    pub fn step(&self, settings: &mut GameSettings, delta_steps: i8) {
        let count = self.slot_count() as isize;
        if count <= 0 {
            return;
        }
        let next = (self.selection(settings) as isize + delta_steps as isize).rem_euclid(count);
        self.apply_slot(settings, next as usize);
    }

    /// Selects the slot nearest a `0.0..=1.0` slider fill (gauge click/drag).
    pub fn set_from_fill(&self, settings: &mut GameSettings, fill: f32) {
        let last = self.slot_count().saturating_sub(1) as f32;
        if last <= 0.0 {
            return;
        }
        let slot = (fill.clamp(0.0, 1.0) * last).round() as usize;
        self.apply_slot(settings, slot);
    }

    /// The selected custom pack, if the current slot is one.
    pub fn selected_pack(&self, settings: &GameSettings) -> Option<&CustomVoicePack> {
        let slot = self.selection(settings);
        if slot < 6 {
            return None;
        }
        self.packs.get(slot - 6)
    }

    /// Audio path for one callout offset in the selected pack.
    pub fn line_for(&self, settings: &GameSettings, offset: u32) -> Option<&str> {
        self.selected_pack(settings)?
            .lines
            .get(&offset)
            .map(String::as_str)
    }

    /// Inserts a pack, replacing any existing pack with the same name (case-insensitive).
    pub fn upsert(&mut self, pack: CustomVoicePack) {
        match self
            .packs
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case(&pack.name))
        {
            Some(existing) => *existing = pack,
            None => self.packs.push(pack),
        }
    }

    /// First unused `CUSTOM n` name for a fresh editor session.
    pub fn next_name(&self) -> String {
        (1..)
            .map(|n| format!("CUSTOM {n}"))
            .find(|name| !self.packs.iter().any(|p| p.name.eq_ignore_ascii_case(name)))
            .unwrap_or_else(|| "CUSTOM".to_string())
    }
}

/// One spoken COM line, emitted after its audio starts so the HUD can show the retail subtitle
/// box. `seconds` is the decoded voice-clip length when known (custom packs return `None`).
#[derive(Message, Debug, Clone, Copy)]
pub struct ComLine {
    pub offset: u32,
    pub seconds: Option<f32>,
}

/// The `aisys.fmg` line id for an `AI_MESSAGE_SOUND_MAP` voice offset, from the generated
/// English table (`sheets/voice.csv`). Offsets 34..40 share the "Enemy AC sighted." row with
/// their weight/leg rows, so both the prefix (33) and the type voice resolve to the full line.
pub fn text_id(offset: u32) -> Option<i32> {
    use acvd_data::generated::ai::LANG_EN_TEXT_AI_AISYS_TSMAP as ROWS;
    if offset == 0 {
        return None;
    }
    ROWS.iter()
        .find(|row| {
            [
                row.data.voice_offset0,
                row.data.voice_offset1,
                row.data.voice_offset2,
            ]
            .contains(&offset)
        })
        .map(|row| row.data.text_id)
        .filter(|id| *id > 0)
        .and_then(|id| i32::try_from(id).ok())
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
    library: Res<VoiceLibrary>,
    players: Query<(&Pilot, &Status, &PlayerHealth)>,
    mut lines: MessageWriter<ComLine>,
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

    let custom = settings.com_voice_custom.is_some();
    let voice = settings.com_voice();
    for offset in want {
        if (voice == 0 && !custom) || state.cooldown > 0.0 {
            break;
        }
        let played = match library.line_for(&settings, offset) {
            Some(path) => cues.custom_voice(&mut commands, &mut assets, path),
            // A custom pack replaces the built-in voice; unset lines stay silent.
            None if custom => Vec::new(),
            None => cues.com_voice(&mut commands, &mut assets, voice, offset),
        };
        if !played.is_empty() {
            state.cooldown = 2.0;
            let seconds = if custom {
                None
            } else {
                cues.com_voice_seconds(voice, offset)
            };
            lines.write(ComLine { offset, seconds });
        }
    }
}

pub struct ComVoicePlugin;

impl Plugin for ComVoicePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(VoiceLibrary::load())
            .init_resource::<ComVoiceState>()
            .add_message::<ComLine>()
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

    /// A two-pack library selects, labels, and resolves lines by saved name.
    fn test_library() -> VoiceLibrary {
        let mut lines = BTreeMap::new();
        lines.insert(msg::ENERGY_30, "C:/sounds/energy.mp3".to_string());
        let mut lib = VoiceLibrary::default();
        lib.upsert(CustomVoicePack {
            name: "ALPHA".to_string(),
            lines,
        });
        lib.upsert(CustomVoicePack {
            name: "BRAVO".to_string(),
            lines: BTreeMap::new(),
        });
        lib
    }

    #[test]
    fn library_slots_cover_builtin_and_custom_packs() {
        let lib = test_library();
        let mut s = GameSettings {
            com_voice: 3,
            com_voice_custom: None,
            ..Default::default()
        };
        assert_eq!(lib.slot_count(), 8);
        assert_eq!(lib.selection(&s), 3);
        assert_eq!(lib.label(&s), "TYPE 3");
        lib.step(&mut s, 1);
        assert_eq!(s.com_voice, 4);
        assert_eq!(s.com_voice_custom, None);
        lib.step(&mut s, 1);
        assert_eq!(s.com_voice, 5);
        lib.step(&mut s, 1);
        assert_eq!(lib.selection(&s), 6);
        assert_eq!(lib.label(&s), "ALPHA");
        assert_eq!(s.com_voice_custom.as_deref(), Some("ALPHA"));
        assert_eq!(
            lib.line_for(&s, msg::ENERGY_30),
            Some("C:/sounds/energy.mp3")
        );
        assert_eq!(lib.line_for(&s, msg::AP_30), None);
        lib.step(&mut s, 1);
        assert_eq!(lib.label(&s), "BRAVO");
        lib.step(&mut s, 1);
        assert_eq!(lib.label(&s), "OFF");
        lib.step(&mut s, -1);
        assert_eq!(lib.label(&s), "BRAVO");
        lib.set_from_fill(&mut s, 1.0);
        assert_eq!(lib.selection(&s), 7);
        lib.set_from_fill(&mut s, 0.0);
        assert_eq!(lib.selection(&s), 0);
    }

    #[test]
    fn library_ignores_deleted_custom_selection() {
        let lib = VoiceLibrary::default();
        let mut s = GameSettings {
            com_voice: 2,
            com_voice_custom: Some("MISSING".to_string()),
            ..Default::default()
        };
        assert_eq!(lib.selection(&s), 2);
        lib.apply_slot(&mut s, 5);
        assert_eq!(s.com_voice, 5);
        assert_eq!(s.com_voice_custom, None);
    }

    #[test]
    fn manifest_round_trips_custom_packs() {
        let manifest = VoiceManifest {
            version: 1,
            packs: test_library().packs,
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let back: VoiceManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.packs.len(), 2);
        assert_eq!(
            back.packs[0].lines.get(&msg::ENERGY_30).map(String::as_str),
            Some("C:/sounds/energy.mp3")
        );
    }
}
