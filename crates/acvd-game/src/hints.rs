//! Device-aware control hints and the retail controller key-guide glyph names.
//!
//! The 360 draws key guides as a button glyph from `fontdef.xml` ID 10 (`s1_xbox`), whose
//! ASCII cells `a..y` are the controller icons (D-pad, ABXY, bumpers/triggers, stick clicks).
//! Traced in `lang/en/menu/aitestsortie.drb.dcx` `AITestKeyGuide` (`font=10`, `Static("o")`)
//! and the `s1_xbox` glyph sheet; see `sheets/keyguide.csv`.

use acvd_render::ui_kit::{HintPart, PadGlyph};

use crate::input::InputDevice;

#[derive(Clone, Copy)]
pub struct PadHint {
    pub icons: &'static [PadGlyph],
    pub label: &'static str,
}

const UP_DOWN: &[PadGlyph] = &[PadGlyph::DPadUp, PadGlyph::DPadDown];
const LEFT_RIGHT: &[PadGlyph] = &[PadGlyph::DPadLeft, PadGlyph::DPadRight];
const LB_RB: &[PadGlyph] = &[PadGlyph::Lb, PadGlyph::Rb];

pub const GARAGE_TOP: &[PadHint] = &[
    PadHint {
        icons: UP_DOWN,
        label: "NAVIGATE",
    },
    PadHint {
        icons: &[PadGlyph::A],
        label: "SELECT",
    },
    PadHint {
        icons: &[],
        label: "START OPTIONS",
    },
    PadHint {
        icons: &[PadGlyph::R3],
        label: "ORBIT",
    },
    PadHint {
        icons: &[PadGlyph::Y],
        label: "VIEW MODE",
    },
];

pub const GARAGE_OPTIONS: &[PadHint] = &[
    PadHint {
        icons: UP_DOWN,
        label: "NAVIGATE",
    },
    PadHint {
        icons: LEFT_RIGHT,
        label: "ADJUST",
    },
    PadHint {
        icons: LB_RB,
        label: "TAB",
    },
    PadHint {
        icons: &[PadGlyph::B],
        label: "CLOSE",
    },
    PadHint {
        icons: &[PadGlyph::R3],
        label: "ORBIT",
    },
];

pub const GARAGE_BROWSE: &[PadHint] = &[
    PadHint {
        icons: UP_DOWN,
        label: "NAVIGATE",
    },
    PadHint {
        icons: &[PadGlyph::A],
        label: "SELECT",
    },
    PadHint {
        icons: &[PadGlyph::B],
        label: "BACK",
    },
    PadHint {
        icons: &[PadGlyph::R3],
        label: "ORBIT",
    },
    PadHint {
        icons: &[PadGlyph::Y],
        label: "VIEW MODE",
    },
];

pub const GARAGE_PAINT: &[PadHint] = &[
    PadHint {
        icons: &[PadGlyph::B],
        label: "BACK",
    },
    PadHint {
        icons: &[PadGlyph::R3],
        label: "ORBIT",
    },
];

pub const GARAGE_DECAL: &[PadHint] = &[PadHint {
    icons: &[PadGlyph::B],
    label: "BACK",
}];

pub const ASSEMBLE: &[PadHint] = &[
    PadHint {
        icons: UP_DOWN,
        label: "PART",
    },
    PadHint {
        icons: LEFT_RIGHT,
        label: "SLOT",
    },
    PadHint {
        icons: LB_RB,
        label: "FILTER",
    },
    PadHint {
        icons: &[PadGlyph::X],
        label: "DETAILS",
    },
    PadHint {
        icons: &[PadGlyph::A],
        label: "EQUIP",
    },
    PadHint {
        icons: &[PadGlyph::R3],
        label: "ORBIT",
    },
    PadHint {
        icons: &[PadGlyph::B],
        label: "BACK",
    },
];

pub const VIEW_MODE: &[PadHint] = &[
    PadHint {
        icons: &[PadGlyph::DPadUp],
        label: "WALK",
    },
    PadHint {
        icons: &[PadGlyph::DPadDown],
        label: "BOOST",
    },
    PadHint {
        icons: &[PadGlyph::Lt],
        label: "L ARM",
    },
    PadHint {
        icons: &[PadGlyph::Rt],
        label: "R ARM",
    },
    PadHint {
        icons: &[PadGlyph::DPadLeft],
        label: "L BAY",
    },
    PadHint {
        icons: &[PadGlyph::DPadRight],
        label: "R BAY",
    },
    PadHint {
        icons: &[PadGlyph::Rb],
        label: "SHOULDER",
    },
    PadHint {
        icons: &[PadGlyph::R3],
        label: "ORBIT",
    },
    PadHint {
        icons: &[],
        label: "START HIDE UI",
    },
    PadHint {
        icons: &[PadGlyph::B],
        label: "RETURN",
    },
];

pub const PAUSE_MAIN: &[PadHint] = &[
    PadHint {
        icons: UP_DOWN,
        label: "MOVE",
    },
    PadHint {
        icons: &[PadGlyph::A],
        label: "SELECT",
    },
    PadHint {
        icons: &[],
        label: "START RESUME",
    },
];

pub const PAUSE_OPTIONS: &[PadHint] = &[
    PadHint {
        icons: UP_DOWN,
        label: "NAVIGATE",
    },
    PadHint {
        icons: LEFT_RIGHT,
        label: "ADJUST",
    },
    PadHint {
        icons: LB_RB,
        label: "TAB",
    },
    PadHint {
        icons: &[PadGlyph::B],
        label: "BACK",
    },
];

pub fn parts(hints: &'static [PadHint]) -> Vec<HintPart<'static>> {
    hints
        .iter()
        .map(|hint| HintPart {
            icons: hint.icons,
            label: hint.label,
        })
        .collect()
}

/// True when the caller should draw [`parts`] instead of its keyboard hint line.
pub fn use_icons(device: InputDevice) -> bool {
    device == InputDevice::Gamepad
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_guide_glyphs_are_distinct_atlas_cells() {
        let glyphs = [
            PadGlyph::DPadUp,
            PadGlyph::DPadDown,
            PadGlyph::DPadLeft,
            PadGlyph::DPadRight,
            PadGlyph::A,
            PadGlyph::B,
            PadGlyph::X,
            PadGlyph::Y,
            PadGlyph::Rb,
            PadGlyph::Rt,
            PadGlyph::Lb,
            PadGlyph::Lt,
            PadGlyph::R3,
            PadGlyph::L3,
        ];
        let mut chars: Vec<char> = glyphs.iter().map(|g| g.glyph()).collect();
        chars.sort_unstable();
        chars.dedup();
        assert_eq!(chars.len(), glyphs.len());
        assert!(chars.iter().all(|c| ('a'..='y').contains(c)));
    }

    #[test]
    fn pad_hint_sets_are_non_empty() {
        for set in [
            GARAGE_TOP,
            GARAGE_OPTIONS,
            GARAGE_BROWSE,
            GARAGE_PAINT,
            GARAGE_DECAL,
            ASSEMBLE,
            VIEW_MODE,
            PAUSE_MAIN,
            PAUSE_OPTIONS,
        ] {
            assert!(!set.is_empty());
            assert!(set.iter().all(|h| !h.label.is_empty()));
        }
    }
}
