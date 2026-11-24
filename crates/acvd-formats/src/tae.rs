//! TimeAct Event (`.tae`) animation event timeline reader (Xbox 360, version `0x0001000B`).
//!
//! Layout verified against the 360 executable (`0x8239d928`, `0x825d3c40`, `0x82a21150`,
//! `0x82b94d10`, AC event dispatcher `0x8289acd0`, loader `0x8284e7c8`) and all 618 `.tae` files
//! on the 360 disc (`sheets/formats.csv`):
//! - Header (`0x00..0x50`): magic `"TAE "`, big-endian flag (`+0x04 = 1`), version
//!   (`+0x08 = 0x0001000B`, or legacy `0x0001000A` on one `_unknown` file), file size (`+0x0c`),
//!   file-data offset (`+0x18`, typically `0x50`).
//! - File data (`+0x50`): `file_id` (`+0x00`), `anim_count` (`+0x04`), `anim_ids_offset`
//!   (`+0x08`), `anim_groups_offset` (`+0x0c`), `anim_data_count` (`+0x14`),
//!   `anim_data_offset` (`+0x18`).
//! - Anim ID table (`anim_ids_offset`): `anim_count` × 16-byte `(u32 id, u32 data_off, 0, 0)`.
//! - Anim data (`0x30` bytes): `event_count` (`+0x00`), `events_offset` (`+0x04`),
//!   `event_group_count` (`+0x08`), `event_groups_offset` (`+0x0c`), `time_count` (`+0x10`),
//!   `times_offset` (`+0x14`), `anim_file_offset` (`+0x18`).
//! - Event header (`16` bytes at `events_offset`): `start_time_off` (`+0x00`), `end_time_off`
//!   (`+0x04`), `event_data_off` (`+0x08`). Time slots store big-endian `u32` 60 Hz frame indices.
//! - Event data (`16` bytes at `event_data_off`): `kind` (`+0x00`), `param_offset` (`+0x04`).
//!
//! Key AC event kinds (`0x8289acd0`):
//! - `0`: Spawn FFX (`params[0]` = `ffx_id`, `params[1]` = `dummy_id`)
//! - `1`, `2`: Play sound cue (`params[0]` = sound category, `params[1]` = cue ID; formatted via
//!   360 table `0x8371c688`)
//! - `103`: Footstep (`params[0]` = foot side `0`/`1`, `params[1]` = step type)
//! - `602`: Play AC sound (`params[0]` = cue ID in category 1 `c%08d`)
//!
//! All integers and floats are big-endian.

use anyhow::{ensure, Result};

use crate::reader::Be;

/// ACVD Xbox 360 TAE format versions (`0x0001000B`, plus legacy `0x0001000A` on one `_unknown` file).
pub const VERSION_1000A: u32 = 0x0001_000A;
pub const VERSION: u32 = 0x0001_000B;

/// Event type IDs dispatched by the 360 AC TAE handler (`0x8289acd0`).
pub const KIND_FFX: u32 = 0;
pub const KIND_SOUND_EXT: u32 = 1;
pub const KIND_SOUND: u32 = 2;
pub const KIND_REATTACH: u32 = 101;
pub const KIND_PURGE: u32 = 102;
pub const KIND_FOOTSTEP: u32 = 103;
pub const KIND_AC_SOUND: u32 = 602;

/// Parsed `.tae` timeline file.
#[derive(Debug, Clone)]
pub struct Tae {
    pub version: u32,
    pub file_id: u32,
    pub animations: Vec<Animation>,
}

impl Tae {
    /// Finds the animation timeline with `anim_id` (matching `AcMotioninfo.anim_id`).
    pub fn animation(&self, anim_id: u32) -> Option<&Animation> {
        self.animations.iter().find(|a| a.id == anim_id)
    }
}

/// One animation entry in a `.tae` file.
#[derive(Debug, Clone)]
pub struct Animation {
    pub id: u32,
    pub events: Vec<Event>,
}

/// One timed event on an animation timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Start frame (60 Hz integer frame index in ACVD TAE).
    pub start_frame: u32,
    /// End frame (60 Hz integer frame index in ACVD TAE).
    pub end_frame: u32,
    /// Event type ID (dispatched by 360 `0x8289acd0`).
    pub kind: u32,
    /// Raw big-endian parameter bytes for this event.
    pub params: Vec<u8>,
}

impl Event {
    /// Reads a big-endian `u32` from `params` at byte offset `off`.
    pub fn param_u32(&self, off: usize) -> Option<u32> {
        let bytes = self.params.get(off..off + 4)?;
        Some(u32::from_be_bytes(bytes.try_into().ok()?))
    }

    /// Reads a big-endian `i32` from `params` at byte offset `off`.
    pub fn param_i32(&self, off: usize) -> Option<i32> {
        self.param_u32(off).map(|v| v as i32)
    }

    /// Reads a big-endian `f32` from `params` at byte offset `off`.
    pub fn param_f32(&self, off: usize) -> Option<f32> {
        self.param_u32(off).map(f32::from_bits)
    }

    /// Reads a big-endian `u16` from `params` at byte offset `off`.
    pub fn param_u16(&self, off: usize) -> Option<u16> {
        let bytes = self.params.get(off..off + 2)?;
        Some(u16::from_be_bytes(bytes.try_into().ok()?))
    }

    /// Reads a `u8` from `params` at byte offset `off`.
    pub fn param_u8(&self, off: usize) -> Option<u8> {
        self.params.get(off).copied()
    }

    /// Decodes a `KIND_FFX` (0) event as `(ffx_id, dummy_id)`.
    pub fn as_ffx(&self) -> Option<(i32, u8)> {
        if self.kind != KIND_FFX {
            return None;
        }
        let ffx_id = self.param_i32(0)?;
        let dummy_id = self.param_u32(4).unwrap_or(0);
        if ffx_id <= 0 || dummy_id > u8::MAX as u32 {
            return None;
        }
        Some((ffx_id, dummy_id as u8))
    }

    /// Decodes a sound event (`KIND_SOUND_EXT` 1, `KIND_SOUND` 2, or `KIND_AC_SOUND` 602) as
    /// `(category, sound_id)`.
    pub fn as_sound(&self) -> Option<(u32, u32)> {
        match self.kind {
            KIND_SOUND_EXT | KIND_SOUND => {
                let cat = self.param_u32(0)?;
                let id = self.param_u32(4)?;
                (id > 0).then_some((cat, id))
            }
            KIND_AC_SOUND => {
                let id = self.param_u32(0)?;
                (id > 0).then_some((1, id))
            }
            _ => None,
        }
    }

    /// Decodes a `KIND_FOOTSTEP` (103) event as `(foot_side, step_kind)`.
    pub fn as_footstep(&self) -> Option<(u32, u32)> {
        if self.kind != KIND_FOOTSTEP {
            return None;
        }
        let side = self.param_u32(0)?;
        let step_kind = self.param_u32(4).unwrap_or(0);
        Some((side, step_kind))
    }

    /// Decodes a `KIND_REATTACH` (101, 360 `0x8289acd0` case `0x65`) event as the
    /// `acattachinfo` row ID (e.g. `13`/`14`/`9`/`10` seat on rack at frame 40;
    /// `11`/`12`/`2`/`3` grab to hand at frame 150 in `own*.tae` anims `2000..2003`).
    pub fn as_reattach(&self) -> Option<u32> {
        (self.kind == KIND_REATTACH).then(|| self.param_u32(0)).flatten()
    }

    /// Decodes a `KIND_PURGE` (102, 360 `0x8289acd0` case `0x66`) event as the purge slot ID
    /// (e.g. `2`/`3`/`11`/`12` at frame 10 in `own*.tae` anims `2010..2013`).
    pub fn as_purge(&self) -> Option<u32> {
        (self.kind == KIND_PURGE).then(|| self.param_u32(0)).flatten()
    }

    /// Decodes a part-animation event (`kind == 1` with part slot `17` = `rack_r` or `18` = `rack_l`,
    /// e.g. `[17/18, 10/20, ..., speed]` in `own*.tae` anims `2000..2003`) as `(part_slot, anim_id)`.
    pub fn as_part_anim(&self) -> Option<(u32, u32)> {
        if self.kind != KIND_SOUND_EXT {
            return None;
        }
        let part_slot = self.param_u32(0)?;
        let anim_id = self.param_u32(4)?;
        if matches!(part_slot, 17 | 18) && anim_id > 0 {
            Some((part_slot, anim_id))
        } else {
            None
        }
    }

    /// Formats a `(category, id)` pair using the 360 sound category format table at `0x8371c688`.
    pub fn format_cue(category: u32, id: u32) -> Option<String> {
        if id == 0 {
            return None;
        }
        let prefix = match category {
            1 | 14 | 15 => 'c',
            2 => 'a',
            3 => 'b',
            4 => 'w',
            5 => 'm',
            6 => 'd',
            _ => return None,
        };
        Some(format!("{prefix}{id:08}"))
    }
}

/// Parses a big-endian ACVD `.tae` file.
pub fn read(data: &[u8]) -> Result<Tae> {
    let b = Be(data);
    ensure!(data.len() >= 0x70, "tae too short: {} bytes", data.len());
    ensure!(&data[0..4] == b"TAE ", "bad tae magic {:?}", &data[0..4]);
    let version = b.u32(0x08)?;
    ensure!(
        version == VERSION || version == VERSION_1000A,
        "unsupported tae version {version:#x}"
    );

    let file_size = b.u32(0x0c)? as usize;
    ensure!(
        file_size <= data.len(),
        "tae file_size {file_size} exceeds buffer {}",
        data.len()
    );

    let file_data_off = b.u32(0x18)? as usize;
    ensure!(
        file_data_off + 0x20 <= data.len(),
        "tae file_data_off {file_data_off:#x} out of range"
    );

    let file_id = b.u32(file_data_off)?;
    let anim_count = b.u32(file_data_off + 0x04)? as usize;
    let anim_ids_off = b.u32(file_data_off + 0x08)? as usize;

    let mut animations = Vec::with_capacity(anim_count);
    for i in 0..anim_count {
        let entry_off = anim_ids_off + i * 0x10;
        let id = b.u32(entry_off)?;
        let anim_off = b.u32(entry_off + 0x04)? as usize;
        ensure!(
            anim_off + 0x1c <= data.len(),
            "tae anim[{i}] data offset {anim_off:#x} out of range"
        );

        let ev_count = b.u32(anim_off)? as usize;
        let ev_off = b.u32(anim_off + 0x04)? as usize;
        let groups_off = b.u32(anim_off + 0x0c)? as usize;

        if ev_count == 0 {
            animations.push(Animation {
                id,
                events: Vec::new(),
            });
            continue;
        }

        struct RawEv {
            start_frame: u32,
            end_frame: u32,
            kind: u32,
            data_off: usize,
            param_off: usize,
        }
        let mut raw = Vec::with_capacity(ev_count);
        for j in 0..ev_count {
            let hdr = ev_off + j * 0x10;
            let start_off = b.u32(hdr)? as usize;
            let end_off = b.u32(hdr + 0x04)? as usize;
            let data_off = b.u32(hdr + 0x08)? as usize;
            let start_frame = b.u32(start_off)?;
            let end_frame = b.u32(end_off)?;
            let kind = b.u32(data_off)?;
            let param_off = b.u32(data_off + 0x04)? as usize;
            raw.push(RawEv {
                start_frame,
                end_frame,
                kind,
                data_off,
                param_off,
            });
        }

        let mut bounds: Vec<usize> = Vec::with_capacity(ev_count * 2 + 2);
        for r in &raw {
            bounds.push(r.data_off);
            if r.param_off != 0 {
                bounds.push(r.param_off);
            }
        }
        if groups_off != 0 {
            bounds.push(groups_off);
        }
        bounds.push(file_size);
        bounds.sort_unstable();
        bounds.dedup();

        let mut events = Vec::with_capacity(ev_count);
        for r in raw {
            let params = if r.param_off != 0 && r.param_off < file_size {
                let end = bounds
                    .iter()
                    .copied()
                    .find(|&x| x > r.param_off)
                    .unwrap_or(file_size)
                    .min(file_size);
                data[r.param_off..end].to_vec()
            } else {
                Vec::new()
            };
            events.push(Event {
                start_frame: r.start_frame,
                end_frame: r.end_frame,
                kind: r.kind,
                params,
            });
        }

        animations.push(Animation { id, events });
    }

    Ok(Tae {
        version,
        file_id,
        animations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_event_helpers() {
        let ffx = Event {
            start_frame: 60,
            end_frame: 61,
            kind: KIND_FFX,
            params: [98u32.to_be_bytes(), 31u32.to_be_bytes()].concat(),
        };
        assert_eq!(ffx.as_ffx(), Some((98, 31)));
        assert_eq!(ffx.as_sound(), None);

        let snd = Event {
            start_frame: 4,
            end_frame: 7,
            kind: KIND_SOUND,
            params: [1u32.to_be_bytes(), 89u32.to_be_bytes()].concat(),
        };
        assert_eq!(snd.as_sound(), Some((1, 89)));
        assert_eq!(Event::format_cue(1, 89).as_deref(), Some("c00000089"));
        assert_eq!(Event::format_cue(4, 34).as_deref(), Some("w00000034"));

        let step = Event {
            start_frame: 19,
            end_frame: 22,
            kind: KIND_FOOTSTEP,
            params: [1u32.to_be_bytes(), 1u32.to_be_bytes()].concat(),
        };
        assert_eq!(step.as_footstep(), Some((1, 1)));
    }

    #[test]
    fn reads_synthetic_tae() {
        // Minimal valid TAE 1.000B with 1 animation (id = 2) and 1 footstep event (frames 19..22).
        let mut buf = vec![0u8; 0x100];
        buf[0..4].copy_from_slice(b"TAE ");
        buf[4] = 1;
        buf[0x08..0x0c].copy_from_slice(&VERSION.to_be_bytes());
        let len = buf.len() as u32;
        buf[0x0c..0x10].copy_from_slice(&len.to_be_bytes());
        buf[0x18..0x1c].copy_from_slice(&0x50u32.to_be_bytes());
        // File data at 0x50
        buf[0x50..0x54].copy_from_slice(&100u32.to_be_bytes()); // file_id
        buf[0x54..0x58].copy_from_slice(&1u32.to_be_bytes()); // anim_count
        buf[0x58..0x5c].copy_from_slice(&0x70u32.to_be_bytes()); // anim_ids_off
        // Anim ID entry at 0x70
        buf[0x70..0x74].copy_from_slice(&2u32.to_be_bytes()); // id = 2
        buf[0x74..0x78].copy_from_slice(&0x80u32.to_be_bytes()); // anim_off
        // Anim data at 0x80 (0x30 bytes)
        buf[0x80..0x84].copy_from_slice(&1u32.to_be_bytes()); // ev_count
        buf[0x84..0x88].copy_from_slice(&0xb8u32.to_be_bytes()); // ev_off
        // Times at 0xb0
        buf[0xb0..0xb4].copy_from_slice(&19u32.to_be_bytes());
        buf[0xb4..0xb8].copy_from_slice(&22u32.to_be_bytes());
        // Event header at 0xb8 (16 bytes)
        buf[0xb8..0xbc].copy_from_slice(&0xb0u32.to_be_bytes());
        buf[0xbc..0xc0].copy_from_slice(&0xb4u32.to_be_bytes());
        buf[0xc0..0xc4].copy_from_slice(&0xc8u32.to_be_bytes());
        // Event data at 0xc8 (16 bytes)
        buf[0xc8..0xcc].copy_from_slice(&KIND_FOOTSTEP.to_be_bytes());
        buf[0xcc..0xd0].copy_from_slice(&0xd8u32.to_be_bytes());
        // Event params at 0xd8 (8 bytes)
        buf[0xd8..0xdc].copy_from_slice(&1u32.to_be_bytes());
        buf[0xdc..0xe0].copy_from_slice(&1u32.to_be_bytes());

        let tae = read(&buf).unwrap();
        assert_eq!(tae.file_id, 100);
        let anim = tae.animation(2).expect("anim 2");
        assert_eq!(anim.events.len(), 1);
        assert_eq!(anim.events[0].start_frame, 19);
        assert_eq!(anim.events[0].end_frame, 22);
        assert_eq!(anim.events[0].as_footstep(), Some((1, 1)));
    }

    #[test]
    fn parses_all_disc_tae_files() {
        let Some(disc) = crate::vfs::test_disc() else {
            return;
        };
        let paths: Vec<String> = disc
            .files()
            .into_iter()
            .filter(|p| p.ends_with(".tae"))
            .collect();
        assert!(
            paths.len() >= 600,
            "expected >= 600 .tae files on 360 ISO, found {}",
            paths.len()
        );
        let mut total_anims = 0usize;
        let mut total_events = 0usize;
        for path in &paths {
            let bytes = crate::vfs::open(&disc, path)
                .unwrap_or_else(|e| panic!("open {path}: {e:#}"));
            let tae = read(&bytes).unwrap_or_else(|e| panic!("parse {path}: {e:#}"));
            total_anims += tae.animations.len();
            total_events += tae.animations.iter().map(|a| a.events.len()).sum::<usize>();
        }
        assert!(total_anims > 0 && total_events > 0);
        // Verify own000100 walk-forward (anim 2) has the two footstep events (frames 19 & 44).
        let own100 = read(&crate::vfs::open(&disc, "tae/ac/motion_ow_disarm/own000100.tae").unwrap())
            .unwrap();
        let walk = own100.animation(2).expect("walk anim 2");
        let steps: Vec<_> = walk
            .events
            .iter()
            .filter_map(|e| e.as_footstep().map(|s| (e.start_frame, s)))
            .collect();
        assert_eq!(steps, vec![(19, (1, 1)), (44, (0, 1))]);
    }

    #[test]
    fn bay_shift_and_purge_tae_timelines() {
        let Some(disc) = crate::vfs::test_disc() else {
            return;
        };
        let own100 = read(
            &crate::vfs::open(&disc, "tae/ac/motion_ow_disarm/own000100.tae").unwrap(),
        )
        .unwrap();

        // Bay shift timelines 2000..2003 (360 0x82849440 / 0x82849450 -> 0x828487b8).
        for (anim_id, dummy, part_slot, rack_anim, seat_id, grab_id) in [
            (2000u32, 1u8, 18u32, 10u32, 13u32, 11u32),
            (2001, 2, 17, 10, 14, 12),
            (2002, 1, 18, 20, 9, 2),
            (2003, 2, 17, 20, 10, 3),
        ] {
            let a = own100.animation(anim_id).unwrap_or_else(|| panic!("missing anim {anim_id}"));
            assert!(a.events.iter().any(|e| {
                e.start_frame == 0
                    && e.as_sound().and_then(|(c, i)| Event::format_cue(c, i)).as_deref()
                        == Some("c00000047")
            }));
            assert!(a.events.iter().any(|e| e.start_frame == 0 && e.as_ffx() == Some((4109, dummy))));
            assert!(a.events.iter().any(|e| {
                e.start_frame == 0 && e.as_part_anim() == Some((part_slot, rack_anim))
            }));
            assert!(a.events.iter().any(|e| e.start_frame == 15 && e.as_ffx() == Some((98, dummy))));
            assert!(a.events.iter().any(|e| e.start_frame == 40 && e.as_reattach() == Some(seat_id)));
            assert!(a.events.iter().any(|e| e.start_frame == 150 && e.as_reattach() == Some(grab_id)));
        }

        // Bay purge timelines 2010..2013 (360 0x82848548 -> 0x82848cc8).
        for (anim_id, purge_slot) in [(2010u32, 2u32), (2011, 3), (2012, 11), (2013, 12)] {
            let a = own100.animation(anim_id).unwrap_or_else(|| panic!("missing anim {anim_id}"));
            assert!(a.events.iter().any(|e| {
                e.start_frame == 10
                    && e.as_sound().and_then(|(c, i)| Event::format_cue(c, i)).as_deref()
                        == Some("c00000045")
            }));
            assert!(a.events.iter().any(|e| e.start_frame == 10 && e.as_purge() == Some(purge_slot)));
        }
    }
}
