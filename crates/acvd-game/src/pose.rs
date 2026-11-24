//! Posing: one joint hierarchy for the assembled AC, driven by the motion set its legs select
//! (`acvd_data::ac_motion`). Every part bone is a joint; bones the clip names take its rotations,
//! keeping the part's own bone lengths, so one clip drives every leg of its type. Clip bones no
//! part has (`master`, `center`, `c_center`, `k_center`) become joints of their own, and part
//! roots hanging from them (`kosi`, `core`) take the clip's translations scaled to the legs.
//! Part roots without a clip bone (weapons, boosters, shoulders) ride the bone carrying the
//! socket they attach to.
//!
//! Joints are solved in FLVER axes and mirrored on X into Bevy's, like the meshes.

use std::collections::HashMap;
use std::sync::Arc;

use acvd_data::generated::ac_unit::{
    AcanimHokanparamSt, PARAM_ACMOTION_BIN, PARAM_MOTIONCOLLATE_BIN,
};
use acvd_data::{find, Asset, SocketRef};
use acvd_formats::vfs::{self, Disc};
use acvd_formats::{ani, flver, jcon, tae};
use acvd_render::{Rig, RigBone};
use anyhow::{Context, Result};
use bevy::math::{Affine3A, Mat3};
use bevy::prelude::*;

use crate::assemble::Placement;

/// `.ani` headers store no frame rate; the game's 60 Hz tick is assumed (30 fps played far too slow).
pub const FRAME_RATE: f32 = 60.0;

/// How fast the lean follows the heading, degrees per second (assumed; the 360 rate is unread).
pub(crate) const WHEEL_RATE: f32 = 540.0;
/// How fast a wheel clip may slide the `center` bone, metres per second. On `a01_055` that
/// bone's translation (its rotation stays identity) carries the whole body and swings about
/// 1.8 m around the wheel. Tracking the heading at `WHEEL_RATE` shoves the mech whenever the
/// strafe direction changes. The limb pose still follows the heading; this only limits the slide.
const CENTER_SHIFT_RATE: f32 = 2.5;

/// Bone-group columns of `param/acanimhokan.bin` after GeneralFrame, by skeleton bone name
/// (`sheets/anim_blend.csv`). A joint takes the column of its nearest ancestor-or-self in the
/// list, GeneralFrame otherwise; `momo` and `sune` name the `l_`/`r_` thigh and shin bones.
const HOKAN_GROUPS: [&str; 7] = [
    "center", "c_center", "k_center", "core", "kosi", "momo", "sune",
];

/// Hokan values per second: milliseconds (at 60 Hz, 2 s idle and 20 s tank fades were far too slow;
/// ms gives 60 ms walk, ~1 s landing, `sheets/anim_blend.csv`, unverified).
const HOKAN_FRAME_RATE: f32 = 1000.0;

/// Bones that receive Core Control procedural aim rotation (360 `0x8289e608` / `0x8289e158`,
/// `param/jcondata.bin` `"Core"`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoreBone {
    #[default]
    None,
    Core,
    Head,
}

/// Core Control state: smoothly rotates the `core` (and slight `head_01` lead) toward the
/// camera/FCS aim direction while the legs face/strafe in the travel direction.
///
/// Limits and turn rates come from `param/jcondata.bin` `"Core"` (`lock = [[-10, 10], [-90, 90]]`
/// deg, `limit_rot_vel = [180, 180]` deg/s; 360 `0x8284a828` -> `0x8289e730` -> `0x8287f448`);
/// engage/release blend times come from `param/acanimparam.bin` (`コア：構え` = 5 frames,
/// `コア：構え解除` = 120 frames; 360 `0x8289e7cc`).
#[derive(Clone, Copy, Debug)]
pub struct CoreControl {
    pub yaw_limits: (f32, f32),
    pub pitch_limits: (f32, f32),
    pub max_yaw_vel: f32,
    pub max_pitch_vel: f32,
    pub raise_secs: f32,
    pub lower_secs: f32,
    pub target_yaw: f32,
    pub target_pitch: f32,
    pub enabled: bool,
    pub yaw: f32,
    pub pitch: f32,
    pub weight: f32,
}

impl Default for CoreControl {
    fn default() -> Self {
        Self {
            yaw_limits: (-90.0_f32.to_radians(), 90.0_f32.to_radians()),
            pitch_limits: (-10.0_f32.to_radians(), 10.0_f32.to_radians()),
            max_yaw_vel: 180.0_f32.to_radians(),
            max_pitch_vel: 180.0_f32.to_radians(),
            raise_secs: 5.0 / FRAME_RATE,
            lower_secs: 120.0 / FRAME_RATE,
            target_yaw: 0.0,
            target_pitch: 0.0,
            enabled: true,
            yaw: 0.0,
            pitch: 0.0,
            weight: 1.0,
        }
    }
}

impl CoreControl {
    /// Loads `"Core"` limits and turn rates from `param/jcondata.bin`, falling back to defaults.
    pub fn from_disc(disc: &Disc) -> Self {
        let mut cc = Self::default();
        if let Some(obj) = vfs::open(disc, "param/jcondata.bin")
            .ok()
            .and_then(|bytes| jcon::read(&bytes).ok())
            .and_then(|controls| {
                controls
                    .into_iter()
                    .find(|c| c.name == "Core")
                    .and_then(|c| c.objects.into_iter().next())
            })
        {
            cc.pitch_limits = (obj.lock[0][0].to_radians(), obj.lock[0][1].to_radians());
            cc.yaw_limits = (obj.lock[1][0].to_radians(), obj.lock[1][1].to_radians());
            if obj.limit_rot_vel[0] > 0.0 {
                cc.max_pitch_vel = obj.limit_rot_vel[0].to_radians();
            }
            if obj.limit_rot_vel[1] > 0.0 {
                cc.max_yaw_vel = obj.limit_rot_vel[1].to_radians();
            }
        }
        cc
    }

    /// Advances Core Control by `dt` seconds: clamps target angles, slews at `limit_rot_vel`,
    /// and eases `weight` up when enabled or down when `b_disable_core_ctrl` suppresses it.
    pub fn step(&mut self, dt: f32) {
        if dt <= 0.0 {
            return;
        }
        if self.enabled {
            self.weight = (self.weight + dt / self.raise_secs.max(1e-4)).min(1.0);
        } else {
            self.weight = (self.weight - dt / self.lower_secs.max(1e-4)).max(0.0);
        }
        let goal_yaw = if self.enabled {
            self.target_yaw.clamp(self.yaw_limits.0, self.yaw_limits.1)
        } else {
            0.0
        };
        let goal_pitch = if self.enabled {
            self.target_pitch
                .clamp(self.pitch_limits.0, self.pitch_limits.1)
        } else {
            0.0
        };
        let max_dy = self.max_yaw_vel * dt;
        let max_dp = self.max_pitch_vel * dt;
        self.yaw += (goal_yaw - self.yaw).clamp(-max_dy, max_dy);
        self.pitch += (goal_pitch - self.pitch).clamp(-max_dp, max_dp);
    }
}

/// Decoded TAE timeline event emitted when playback crosses an event's `start_frame`
/// (360 AC TAE dispatcher `0x8289acd0`).
#[derive(Clone, Debug, PartialEq)]
pub enum TaeFired {
    /// TAE kind `103` (`0x67`): footstep contact (`side`: 0/1, `kind`: step variant).
    Footstep { side: u32, kind: u32 },
    /// TAE kinds `1`, `2`, `602`: direct sound cue formatted via 360 table `0x8371c688`.
    Sound { cue: String },
    /// TAE kind `0`: spawn FFX effect `ffx_id` on part dummy socket `dummy_id` at `scale`
    /// (`1.5` for bay-shift arm-overlay TAE `2000..2003`, verified in Xenia `bay_switch.jsonl`).
    Ffx { ffx_id: i32, dummy_id: u8, scale: f32 },
    /// TAE kind `101` (`0x65`): reattach weapon (`acattachinfo` row ID; frame 40 seat, frame 150 grab).
    Reattach { id: u32 },
    /// TAE kind `102` (`0x66`): purge weapon (`2`/`3`/`11`/`12` at frame 10 of `2010..2013`).
    Purge { id: u32 },
}

/// Active per-arm overlay animation layer (360 `0x828487b8` / `0x82848cc8` -> `0x82854a50`
/// layer `0x0a` right arm `r_kata`, layer `0x0b` left arm `l_kata`). Plays bay shift (`300`/`301`
/// + TAE `2000..2003`) and bay purge (`302`/`303` + TAE `2010..2013`) independently on each arm
/// without freezing the other arm or lower-body locomotion.
pub struct ArmLayer {
    pub anim_id: u16,
    pub tae_id: u32,
    pub clip: Arc<ani::Anim>,
    pub frame: f32,
    pub speed: f32,
    pub fade: Fade,
}

impl ArmLayer {
    pub fn finished(&self) -> bool {
        self.frame >= self.clip.frames as f32
    }
}

/// Active upper-body animation layer playing over lower-body locomotion (`c_center` / `core`
/// subtree; `AC_MOTIONINFO` `upper_priority` / `b_apply_lower == 0`).
pub struct UpperLayer {
    pub anim_id: u16,
    pub clip: Arc<ani::Anim>,
    pub frame: f32,
    pub looping: bool,
    pub speed: f32,
    pub priority: u8,
    pub fade: Fade,
}

impl UpperLayer {
    pub fn finished(&self) -> bool {
        !self.looping && self.frame >= self.clip.frames.saturating_sub(1) as f32
    }
}

/// A joint the clip drives: clip bone `bone`; `absolute` joints take the clip's translation
/// (scaled to the legs), the others keep `bind` (their part's bone length) plus the clip's
/// change from rest. `group` indexes `Fade::secs`; `from` is the pose the running crossfade
/// started at, taken when `generation` falls behind the motion's.
#[derive(Component)]
pub struct Driven {
    pub bone: usize,
    pub absolute: bool,
    pub bind: Vec3,
    pub upper: bool,
    /// `Some(0)` for right arm (`r_kata` subtree, 360 layer `0x0a`), `Some(1)` for left arm
    /// (`l_kata` subtree, 360 layer `0x0b`).
    pub arm_side: Option<u8>,
    pub core_bone: CoreBone,
    group: usize,
    /// Last pre-Core-Control animated pose, captured by crossfade transitions so procedural
    /// `CoreControl` rotation never compounds across clip changes.
    base: Transform,
    from: Transform,
    generation: u32,
    upper_from: Transform,
    upper_gen: u32,
    arm_from: Transform,
    arm_gen: u32,
}

/// Crossfade from the pose shown at the last clip change into the new clip: per bone group
/// (GeneralFrame, then `HOKAN_GROUPS`), seconds to reach it; linear in time.
#[derive(Clone, Copy, Default)]
pub struct Fade {
    pub generation: u32,
    pub secs: [f32; 8],
    pub t: f32,
}

impl Fade {
    /// The crossfade times of one `ACANIM_HOKANPARAM_ST` row. `booster` is the nozzle clips
    /// (`sheets/booster_anim.csv`); the other eight are the body groups.
    pub fn booster(row: &AcanimHokanparamSt) -> f32 {
        f32::from(row.booster_frame.max(0)) / HOKAN_FRAME_RATE
    }

    /// The body crossfade times of one `ACANIM_HOKANPARAM_ST` row.
    pub fn of(row: &AcanimHokanparamSt) -> [f32; 8] {
        [
            row.general_frame,
            row.center_frame,
            row.c_center_frame,
            row.k_center_frame,
            row.core_frame,
            row.kosi_frame,
            row.momo_frame,
            row.sune_frame,
        ]
        .map(|f| f32::from(f.max(0)) / HOKAN_FRAME_RATE)
    }
}

/// The hokan column of clip bone `bone`: its nearest ancestor-or-self named in `HOKAN_GROUPS`.
fn hokan_group(rest: &[ani::Rest], bone: usize) -> usize {
    let mut at = Some(bone);
    while let Some(i) = at {
        let name = rest[i].name.as_str();
        let side = name.strip_prefix("l_").or_else(|| name.strip_prefix("r_"));
        let hit = HOKAN_GROUPS.iter().position(|g| {
            name == *g || (matches!(*g, "momo" | "sune") && side.is_some_and(|s| s.starts_with(g)))
        });
        if let Some(g) = hit {
            return g + 1;
        }
        at = rest[i].parent.map(usize::from);
    }
    0
}

/// Whether clip bone `bone` belongs to the upper-body subtree (`c_center` / `core` and its
/// descendants: head, shoulders, arms, weapons, core boosters). Lower-body bones (`master`,
/// `center`, `k_center`, `kosi`, legs) return `false`.
pub fn is_upper_body(rest: &[ani::Rest], bone: usize) -> bool {
    let mut at = Some(bone);
    while let Some(i) = at {
        let name = rest[i].name.as_str();
        if matches!(name, "c_center" | "core") {
            return true;
        }
        at = rest[i].parent.map(usize::from);
    }
    false
}

/// Which arm subtree `bone` belongs to: `Some(0)` for right arm (`r_kata` and descendants,
/// 360 layer `0x0a`), `Some(1)` for left arm (`l_kata` and descendants, 360 layer `0x0b`).
pub fn arm_side_of(rest: &[ani::Rest], bone: usize) -> Option<u8> {
    let mut at = Some(bone);
    while let Some(i) = at {
        match rest[i].name.as_str() {
            "r_kata" => return Some(0),
            "l_kata" => return Some(1),
            _ => {}
        }
        at = rest[i].parent.map(usize::from);
    }
    None
}

/// Identifies the `core` and `head_01`/`head` bones that receive Core Control rotation
/// (360 `0x8289e608` / `0x8289e158`).
pub fn core_bone_of(rest: &[ani::Rest], bone: usize) -> CoreBone {
    match rest.get(bone).map(|r| r.name.as_str()) {
        Some("core") => CoreBone::Core,
        Some("head_01") => CoreBone::Head,
        Some("head") if !rest.iter().any(|r| r.name == "head_01") => CoreBone::Head,
        _ => CoreBone::None,
    }
}

/// Returns all TAE events whose `start_frame` is crossed as playback moves from `prev` to `curr`
/// on a clip of length `frames` (`looping` handles wrap-around past `frames`).
pub fn crossed_events(
    anim: &tae::Animation,
    prev: f32,
    curr: f32,
    frames: f32,
    looping: bool,
) -> Vec<TaeFired> {
    crossed_events_scaled(anim, prev, curr, frames, looping, 1.0)
}

/// Same as [`crossed_events`], applying `ffx_scale` to emitted `TaeFired::Ffx` events.
pub fn crossed_events_scaled(
    anim: &tae::Animation,
    prev: f32,
    curr: f32,
    frames: f32,
    looping: bool,
    ffx_scale: f32,
) -> Vec<TaeFired> {
    if anim.events.is_empty() || frames <= 0.0 || (curr - prev).abs() < 1e-6 {
        return Vec::new();
    }
    let wrapped = looping && curr < prev;
    let mut out = Vec::new();
    for ev in &anim.events {
        let f = ev.start_frame as f32;
        let hit = if wrapped {
            (f > prev && f < frames) || (f >= 0.0 && f <= curr)
        } else {
            (f > prev && f <= curr) || (prev == 0.0 && ev.start_frame == 0 && curr > 0.0)
        };
        if !hit {
            continue;
        }
        if let Some((side, kind)) = ev.as_footstep() {
            out.push(TaeFired::Footstep { side, kind });
        } else if let Some(id) = ev.as_reattach() {
            out.push(TaeFired::Reattach { id });
        } else if let Some(id) = ev.as_purge() {
            out.push(TaeFired::Purge { id });
        } else if let Some((cat, id)) = ev.as_sound() {
            if let Some(cue) = tae::Event::format_cue(cat, id) {
                out.push(TaeFired::Sound { cue });
            }
        } else if let Some((ffx_id, dummy_id)) = ev.as_ffx() {
            out.push(TaeFired::Ffx {
                ffx_id,
                dummy_id,
                scale: ffx_scale,
            });
        }
    }
    out
}

/// Parses the trailing numeric anim ID from an entry name like `"a01_002.ani"`.
fn parse_anim_id(entry: &str) -> u16 {
    entry
        .strip_suffix(".ani")
        .and_then(|s| s.rsplit('_').next())
        .and_then(|n| n.parse::<u16>().ok())
        .unwrap_or(0)
}

/// Parses `<prefix>_<id:03>.ani` strictly (matching `Motion::play`'s entry format).
fn parse_prefixed_anim_id(prefix: &str, entry: &str) -> Option<u16> {
    let rest = entry
        .strip_prefix(prefix)?
        .strip_prefix('_')?
        .strip_suffix(".ani")?;
    if rest.len() < 3 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse::<u16>().ok()
}

/// The motion set playing on the shown AC.
#[derive(Resource)]
pub struct Motion {
    pub set: &'static str,
    pub skeleton: ani::Anim,
    /// Clip name prefix of the set (`a01`, or `a00` for tank and quad sets), from its skeleton clip.
    pub prefix: &'static str,
    pub clips: Vec<&'static Asset>,
    pub index: usize,
    pub clip: Arc<ani::Anim>,
    /// `AcMotioninfo.anim_id` of the currently playing lower-body clip (used for TAE lookup).
    pub current_anim_id: u16,
    pub frame: f32,
    pub playing: bool,
    /// Non-looping clips hold their last frame.
    pub looping: bool,
    /// Playback rate relative to `FRAME_RATE`.
    pub speed: f32,
    /// Direction-wheel clips (dash / air-move / glide lean, 360 frames, a key every 45 = one of the eight
    /// directions, frame = heading in degrees clockwise from forward): the heading to pose, which
    /// replaces time playback. Cleared by select.
    pub wheel: Option<f32>,
    /// Wheel clips: 0..1 weight of the lean pose over the pose shown at the clip change (set from
    /// the AC's speed, so the lean builds up as it accelerates) instead of the timed crossfade.
    pub lean: f32,
    /// Smoothed `center` translation while a wheel clip plays, so a heading change does not
    /// shove the body. `center_ready` is false until the first wheel frame plants it.
    center_shift: Vec3,
    center_ready: bool,
    /// Hold the clip's root bone (`master`) at rest: its motion (the turn clips yaw it) is
    /// applied by whoever moves the AC instead.
    pub in_place: bool,
    /// Leg bone length over the clip body's, applied to absolute translations.
    pub scale: f32,
    /// Crossfade into the current clip; clip changes through `select` cut instead.
    pub fade: Fade,
    /// Optional upper-body clip layered over lower-body locomotion (`c_center` / `core` subtree).
    pub upper: Option<UpperLayer>,
    /// Smooth crossfade back to the lower-body clip after `upper` clears.
    upper_return_fade: Fade,
    /// Per-arm overlay clips (`[0]` right arm `r_kata`, `[1]` left arm `l_kata`; 360 `0x828487b8`
    /// -> `0x82854a50` layers `0x0a`/`0x0b`) for independent bay shifts and purges.
    pub arm_layers: [Option<ArmLayer>; 2],
    /// Smooth crossfade back to the body/upper clip after each arm overlay clears.
    arm_return_fade: [Fade; 2],
    /// Core Control procedural aim yaw/pitch on `core` (+ slight `head_01` lead).
    pub core_ctrl: CoreControl,
    /// Loaded `tae/ac/motion_ow_disarm/own{:06}.tae` timeline for this legs motion set.
    pub tae: Option<tae::Tae>,
    /// Optional `motion_ow_equip` binder path (`0x8284e9d8`, from `MotionCollateSt`) when the AC
    /// equips an Overed Weapon.
    #[allow(dead_code)]
    pub ow_equip_set: Option<String>,
    /// Clips inside `motion_ow_equip` (`a01_200.ani`..`a01_258.ani`).
    pub ow_equip_clips: Vec<&'static Asset>,
    /// Loaded `tae/ac/motion_ow_equip/own{:06}.tae` timeline (`0x8284e7c8`).
    pub ow_equip_tae: Option<tae::Tae>,
    /// True while the Overed Weapon is armed/deployed (`motion_ow_equip` bank active).
    pub ow_bank_active: bool,
    /// True when the currently loaded lower-body `self.clip` came from `ow_equip_clips`.
    pub playing_ow_clip: bool,
    /// `MotionCollateSt.sound_param_id` for leg-specific footstep cues in `acv2_se_ac.fev`.
    pub sound_param_id: u32,
    /// TAE events fired during the most recent `pose::animate` tick.
    pub tae_events: Vec<TaeFired>,
    clip_by_id: HashMap<u16, usize>,
    ow_clip_by_id: HashMap<u16, &'static Asset>,
    cache: HashMap<usize, Arc<ani::Anim>>,
    ow_cache: HashMap<u16, Arc<ani::Anim>>,
}

impl Motion {
    pub fn name(&self) -> &'static str {
        if self.playing_ow_clip {
            if let Some(a) = self.ow_clip_by_id.get(&self.current_anim_id) {
                return a.entry;
            }
        }
        self.clips.get(self.index).map_or("", |a| a.entry)
    }

    /// True if the `motion_ow_equip` bank is loaded and contains clip `anim_id`.
    pub fn has_ow_clip(&self, anim_id: u16) -> bool {
        self.ow_clip_by_id.contains_key(&anim_id)
    }

    /// Switches between `motion_ow_disarm` and `motion_ow_equip` (`0x8284e9d8`).
    pub fn set_ow_bank(&mut self, active: bool) {
        self.ow_bank_active = active && !self.ow_equip_clips.is_empty();
    }

    fn load_ow_clip(&mut self, disc: &Disc, anim_id: u16) -> Result<Arc<ani::Anim>> {
        if let Some(c) = self.ow_cache.get(&anim_id) {
            return Ok(Arc::clone(c));
        }
        let asset = self
            .ow_clip_by_id
            .get(&anim_id)
            .copied()
            .with_context(|| {
                format!(
                    "motion_ow_equip has no {}_{anim_id:03}.ani",
                    self.prefix
                )
            })?;
        let clip = Arc::new(ani::read(&vfs::open(disc, &asset.path())?)?);
        anyhow::ensure!(
            clip.bones.len() == self.skeleton.bones.len(),
            "{} has {} bones, the skeleton {}",
            asset.entry,
            clip.bones.len(),
            self.skeleton.bones.len()
        );
        self.ow_cache.insert(anim_id, Arc::clone(&clip));
        Ok(clip)
    }

    fn load_clip(&mut self, disc: &Disc, index: usize) -> Result<Arc<ani::Anim>> {
        if let Some(c) = self.cache.get(&index) {
            return Ok(Arc::clone(c));
        }
        let asset = self.clips.get(index).context("no such clip")?;
        let clip = Arc::new(ani::read(&vfs::open(disc, &asset.path())?)?);
        anyhow::ensure!(
            clip.bones.len() == self.skeleton.bones.len(),
            "{} has {} bones, the skeleton {}",
            asset.entry,
            clip.bones.len(),
            self.skeleton.bones.len()
        );
        self.cache.insert(index, Arc::clone(&clip));
        Ok(clip)
    }

    /// Loads clip `index`; clips whose bone count differs from the skeleton's are refused.
    pub fn select(&mut self, disc: &Disc, index: usize) -> Result<()> {
        let clip = self.load_clip(disc, index)?;
        self.clip = clip;
        self.playing_ow_clip = false;
        self.current_anim_id = self
            .clips
            .get(index)
            .map_or(0, |a| parse_anim_id(a.entry));
        (self.index, self.frame, self.looping, self.speed, self.wheel) =
            (index, 0.0, true, 1.0, None);
        self.center_ready = false;
        self.fade = Fade {
            generation: self.fade.generation.wrapping_add(1),
            secs: [0.0; 8],
            t: 0.0,
        };
        Ok(())
    }

    /// Plays clip `anim_id` of the set (`<prefix>_<anim_id>.ani`) from its first frame, unless
    /// it is already playing; a new clip crossfades in over `fade` seconds per bone group.
    /// When `ow_bank_active` is true (or `anim_id >= 200`) and `motion_ow_equip` contains
    /// `anim_id`, plays from the `motion_ow_equip` bank (`0x8284e9d8`).
    pub fn play(
        &mut self,
        disc: &Disc,
        anim_id: u16,
        looping: bool,
        speed: f32,
        fade: [f32; 8],
    ) -> Result<()> {
        if (self.ow_bank_active || anim_id >= 200) && self.has_ow_clip(anim_id) {
            if !self.playing_ow_clip || self.current_anim_id != anim_id {
                self.clip = self.load_ow_clip(disc, anim_id)?;
                self.playing_ow_clip = true;
                self.current_anim_id = anim_id;
                self.frame = 0.0;
                self.wheel = None;
                self.center_ready = false;
                self.fade = Fade {
                    generation: self.fade.generation.wrapping_add(1),
                    secs: fade,
                    t: 0.0,
                };
            } else if !looping && self.wheel.is_none() {
                self.frame = 0.0;
            }
            (self.looping, self.speed, self.playing) = (looping, speed, true);
            return Ok(());
        }
        let index = *self
            .clip_by_id
            .get(&anim_id)
            .with_context(|| format!("{} has no {}_{anim_id:03}.ani", self.set, self.prefix))?;
        if self.playing_ow_clip || index != self.index {
            self.select(disc, index)?;
            self.fade.secs = fade;
        } else if !looping && self.wheel.is_none() {
            // A one-shot replays from the start. A wheel clip (dash, air move, glide) is marked
            // non-looping in acmotion but stays on this anim across headings; restarting it
            // every direction change snaps the pose back to frame 0.
            self.frame = 0.0;
        }
        self.current_anim_id = anim_id;
        (self.looping, self.speed, self.playing) = (looping, speed, true);
        Ok(())
    }

    /// Plays clip `anim_id` on the upper-body layer (`c_center` / `core` subtree) without
    /// interrupting lower-body locomotion. Higher-or-equal `priority` replaces an existing
    /// upper-body clip (`AC_MOTIONINFO.upper_priority`).
    pub fn play_upper(
        &mut self,
        disc: &Disc,
        anim_id: u16,
        looping: bool,
        speed: f32,
        fade: [f32; 8],
        priority: u8,
    ) -> Result<()> {
        if let Some(ref u) = self.upper {
            if u.priority > priority && !u.finished() {
                return Ok(());
            }
        }
        let clip = if (self.ow_bank_active || anim_id >= 200) && self.has_ow_clip(anim_id) {
            self.load_ow_clip(disc, anim_id)?
        } else {
            let index = *self
                .clip_by_id
                .get(&anim_id)
                .with_context(|| format!("{} has no {}_{anim_id:03}.ani", self.set, self.prefix))?;
            self.load_clip(disc, index)?
        };
        let gen = self
            .upper
            .as_ref()
            .map_or(self.upper_return_fade.generation, |u| u.fade.generation)
            .wrapping_add(1);
        self.upper = Some(UpperLayer {
            anim_id,
            clip,
            frame: 0.0,
            looping,
            speed,
            priority,
            fade: Fade {
                generation: gen,
                secs: fade,
                t: 0.0,
            },
        });
        Ok(())
    }

    /// Clears the upper-body layer and crossfades upper-body joints back into the lower-body
    /// clip over `fade_secs` seconds.
    pub fn clear_upper(&mut self, fade_secs: f32) {
        if let Some(u) = self.upper.take() {
            self.upper_return_fade = Fade {
                generation: u.fade.generation.wrapping_add(1),
                secs: [fade_secs.max(0.0); 8],
                t: 0.0,
            };
        }
    }

    /// Whether no upper-body clip is active, or the active non-looping upper clip reached its end.
    pub fn upper_finished(&self) -> bool {
        self.upper.as_ref().is_none_or(UpperLayer::finished)
    }

    /// Plays clip `anim_id` on one arm's overlay layer (`side == 0` right `r_kata` / 360 layer
    /// `0x0a`; `side == 1` left `l_kata` / 360 layer `0x0b`), firing TAE timeline `tae_id`
    /// (`2000..2003` bay shift, `2010..2013` bay purge).
    pub fn play_arm(
        &mut self,
        disc: &Disc,
        side: usize,
        anim_id: u16,
        tae_id: u32,
        speed: f32,
        fade: [f32; 8],
    ) -> Result<()> {
        if side >= 2 {
            return Ok(());
        }
        let index = *self
            .clip_by_id
            .get(&anim_id)
            .with_context(|| format!("{} has no {}_{anim_id:03}.ani", self.set, self.prefix))?;
        let clip = self.load_clip(disc, index)?;
        let gen = self.arm_layers[side]
            .as_ref()
            .map_or(self.arm_return_fade[side].generation, |a| a.fade.generation)
            .wrapping_add(1);
        self.arm_layers[side] = Some(ArmLayer {
            anim_id,
            tae_id,
            clip,
            frame: 0.0,
            speed,
            fade: Fade {
                generation: gen,
                secs: fade,
                t: 0.0,
            },
        });
        Ok(())
    }

    /// Clears arm overlay `side` (`0` right, `1` left) and crossfades that arm back to the
    /// underlying pose over `fade_secs` seconds.
    pub fn clear_arm(&mut self, side: usize, fade_secs: f32) {
        if side >= 2 {
            return;
        }
        if let Some(a) = self.arm_layers[side].take() {
            self.arm_return_fade[side] = Fade {
                generation: a.fade.generation.wrapping_add(1),
                secs: [fade_secs.max(0.0); 8],
                t: 0.0,
            };
        }
    }

    /// Whether arm overlay `side` is inactive or has reached its end frame.
    pub fn arm_finished(&self, side: usize) -> bool {
        self.arm_layers
            .get(side)
            .and_then(|o| o.as_ref())
            .is_none_or(ArmLayer::finished)
    }

    /// Whether a non-looping clip has reached its last frame.
    pub fn finished(&self) -> bool {
        !self.looping && self.frame >= self.clip.frames.saturating_sub(1) as f32
    }
}

/// A part's loaded meshes and skeleton; `index` is its placement's index in the assembly.
pub struct Loaded<'a> {
    pub index: usize,
    pub placement: &'a Placement,
    pub meshes: Vec<acvd_render::LoadedMesh>,
    pub rig: Rig,
}

struct Spec {
    parent: Option<usize>,
    bind: Affine3A,
    bone: Option<usize>,
    virtual_bone: bool,
}

/// The rig to spawn: joints in parent-first order and, per part, the joints its meshes skin to.
pub struct Built {
    joints: Vec<Spec>,
    /// Per loaded part: its first joint (bone 0) and the extra unboned joint's bind.
    parts: Vec<(usize, Affine3A)>,
    /// Hokan bone group per clip bone.
    groups: Vec<usize>,
    /// Upper-body flag per clip bone (`c_center` / `core` subtree).
    upper: Vec<bool>,
    /// Arm subtree tag per clip bone (`Some(0)` right `r_kata`, `Some(1)` left `l_kata`).
    arm_side: Vec<Option<u8>>,
    /// Core Control bone tag per clip bone (`core` / `head_01`).
    core_bones: Vec<CoreBone>,
    pub motion: Option<Motion>,
}

impl Built {
    /// Rest local of part bone `bone` and the joint index of its parent, if it has one.
    /// The local is the same transform [`spawn`] writes, in Bevy axes.
    pub fn mount_local(&self, part: usize, bone: usize) -> Option<(Option<usize>, Transform)> {
        let (first, _) = *self.parts.get(part)?;
        let spec = self.joints.get(first + bone)?;
        let parent_bind = spec
            .parent
            .map_or(Affine3A::IDENTITY, |p| self.joints[p].bind);
        let local = parent_bind.inverse() * spec.bind;
        let (s, r, t) = local.to_scale_rotation_translation();
        Some((spec.parent, to_transform(t, r, s)))
    }

    /// World-space (Bevy axes) AABB of part `part_idx` in the static frame-0 standing pose
    /// produced by [`spawn_static`]. Used by the Garage `PARTS VIEW` preview to center and
    /// fit-scale posed arm parts cleanly.
    pub fn static_part_bounds(&self, part_idx: usize, part: &Loaded) -> Option<(Vec3, Vec3)> {
        use bevy::mesh::VertexAttributeValues;

        let &(first, unboned) = self.parts.get(part_idx)?;
        let n_joints = self.joints.len();
        let locals: Vec<Mat4> = self
            .joints
            .iter()
            .map(|spec| {
                let parent_bind = spec
                    .parent
                    .map_or(Affine3A::IDENTITY, |p| self.joints[p].bind);
                let local = parent_bind.inverse() * spec.bind;
                let (s, r, t) = local.to_scale_rotation_translation();
                let mut transform = to_transform(t, r, s);
                if let (Some(bone), Some(m)) = (spec.bone, self.motion.as_ref()) {
                    if let (Some(rest), Some(cbone)) = (
                        m.skeleton.bones.get(bone).and_then(|b| b.rest.as_ref()),
                        m.clip.bones.get(bone),
                    ) {
                        let cbone = if m.in_place && rest.parent.is_none() {
                            &m.skeleton.bones[bone]
                        } else {
                            cbone
                        };
                        let rotation = Quat::from_array(
                            cbone
                                .track
                                .rotation(0.0)
                                .unwrap_or_else(|| ani::euler_quat(rest.euler)),
                        );
                        let ct =
                            Vec3::from(cbone.track.translation(0.0).unwrap_or(rest.translation));
                        let cs = Vec3::from(cbone.track.scale(0.0).unwrap_or(rest.scale));
                        let absolute = spec.parent.is_none_or(|p| self.joints[p].virtual_bone);
                        let translation = if absolute {
                            ct * m.scale
                        } else {
                            t + (ct - Vec3::from(rest.translation)) * m.scale
                        };
                        transform = to_transform(translation, rotation, cs);
                    }
                }
                transform.to_matrix()
            })
            .collect();

        let mut worlds: Vec<Option<Mat4>> = vec![None; n_joints];
        fn resolve(
            j: usize,
            joints: &[Spec],
            locals: &[Mat4],
            worlds: &mut [Option<Mat4>],
            depth: usize,
        ) -> Mat4 {
            if let Some(w) = worlds[j] {
                return w;
            }
            if depth > 64 {
                return locals[j];
            }
            let w = match joints[j].parent {
                Some(p) => resolve(p, joints, locals, worlds, depth + 1) * locals[j],
                None => locals[j],
            };
            worlds[j] = Some(w);
            w
        }

        let n = part.rig.bones.len();
        let mut skins: Vec<Mat4> = Vec::with_capacity(n + 1);
        for b in 0..n {
            let w = resolve(first + b, &self.joints, &locals, &mut worlds, 0);
            let inv_bind = Mat4::from(mirror(self.joints[first + b].bind).inverse());
            skins.push(w * inv_bind);
        }
        let extra_w = to_transform(unboned.translation.into(), Quat::IDENTITY, Vec3::ONE).to_matrix();
        skins.push(extra_w * Mat4::from(mirror(unboned).inverse()));

        let mut bmin = Vec3::splat(f32::INFINITY);
        let mut bmax = Vec3::splat(f32::NEG_INFINITY);
        for lm in &part.meshes {
            let Some(VertexAttributeValues::Float32x3(positions)) =
                lm.mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                continue;
            };
            let joints_attr = match lm.mesh.attribute(Mesh::ATTRIBUTE_JOINT_INDEX) {
                Some(VertexAttributeValues::Uint16x4(j)) => Some(j.as_slice()),
                _ => None,
            };
            let weights_attr = match lm.mesh.attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT) {
                Some(VertexAttributeValues::Float32x4(w)) => Some(w.as_slice()),
                _ => None,
            };
            for (vi, &p) in positions.iter().enumerate() {
                let p = Vec3::from_array(p);
                let posed = if let (Some(js), Some(ws)) = (joints_attr, weights_attr) {
                    let mut acc = Vec3::ZERO;
                    let mut wsum = 0.0f32;
                    for k in 0..4 {
                        let w = ws[vi][k];
                        if w > 0.0 {
                            if let Some(&m) = skins.get(js[vi][k] as usize) {
                                acc += m.transform_point3(p) * w;
                                wsum += w;
                            }
                        }
                    }
                    if wsum > 1e-4 { acc / wsum } else { p }
                } else {
                    skins.last().map_or(p, |m| m.transform_point3(p))
                };
                bmin = bmin.min(posed);
                bmax = bmax.max(posed);
            }
        }
        (bmin.is_finite() && bmax.is_finite()).then_some((bmin, bmax))
    }

    /// `child` bone's rest transform in `parent` bone's space, in Bevy axes.
    pub fn local_against(
        &self,
        child_part: usize,
        child_bone: usize,
        parent_part: usize,
        parent_bone: usize,
    ) -> Option<Transform> {
        let (child_first, _) = *self.parts.get(child_part)?;
        let (parent_first, _) = *self.parts.get(parent_part)?;
        let child = self.joints.get(child_first + child_bone)?;
        let parent = self.joints.get(parent_first + parent_bone)?;
        let local = parent.bind.inverse() * child.bind;
        let (s, r, t) = local.to_scale_rotation_translation();
        Some(to_transform(t, r, s))
    }

    /// Computes the local Bevy `Transform` of `(child_part, child_bone)` when its root is moved
    /// from `stowed_place` to `deployed_place` and re-parented under `(parent_part, parent_bone)`
    /// (used for OW root `"60"` -> arm dummy `60` re-attachment, `acattachinfo` record 1).
    pub fn local_for_reattach(
        &self,
        child_part: usize,
        child_bone: usize,
        stowed_place: acvd_render::RootPlace,
        deployed_place: acvd_render::RootPlace,
        parent_part: usize,
        parent_bone: usize,
    ) -> Option<Transform> {
        let (child_first, _) = *self.parts.get(child_part)?;
        let (parent_first, _) = *self.parts.get(parent_part)?;
        let child = self.joints.get(child_first + child_bone)?;
        let parent = self.joints.get(parent_first + parent_bone)?;
        let place_affine = |p: acvd_render::RootPlace| {
            Affine3A::from_mat3_translation(
                Mat3::from_cols(Vec3::from(p.x), Vec3::from(p.y), Vec3::from(p.z)),
                Vec3::from(p.t),
            )
        };
        let deployed_world = place_affine(deployed_place)
            * place_affine(stowed_place).inverse()
            * child.bind;
        let local = parent.bind.inverse() * deployed_world;
        let (s, r, t) = local.to_scale_rotation_translation();
        Some(to_transform(t, r, s))
    }
}

pub fn affine(x: &flver::Xform) -> Affine3A {
    let m = &x.m;
    Affine3A::from_mat3_translation(
        Mat3::from_cols(
            Vec3::new(m[0][0], m[1][0], m[2][0]),
            Vec3::new(m[0][1], m[1][1], m[2][1]),
            Vec3::new(m[0][2], m[1][2], m[2][2]),
        ),
        Vec3::from(x.t),
    )
}

pub(crate) fn mirror(a: Affine3A) -> Affine3A {
    let m = Affine3A::from_scale(Vec3::new(-1.0, 1.0, 1.0));
    m * a * m
}

pub(crate) fn to_transform(t: Vec3, r: Quat, s: Vec3) -> Transform {
    Transform {
        translation: Vec3::new(-t.x, t.y, t.z),
        rotation: Quat::from_xyzw(r.x, -r.y, -r.z, r.w),
        scale: s,
    }
}

/// Opens the motion set the placements select (the first that has one), its skeleton clip,
/// its `motion_ow_disarm` TAE timeline (360 `0x8284e7c8`), and `"Core"` limits from
/// `param/jcondata.bin`.
fn motion(disc: &Disc, parts: &[Loaded]) -> Result<Option<Motion>> {
    let Some((leg_part, m)) = parts.iter().find_map(|p| {
        acvd_data::ac_motion(p.placement.category, p.placement.part)
            .map(|m| (p.placement.part, m))
    }) else {
        return Ok(None);
    };
    let skeleton = ani::read(&vfs::open(disc, &format!("{}|{}", m.set, m.skeleton))?)
        .with_context(|| format!("{}|{}", m.set, m.skeleton))?;
    let mut clips: Vec<&'static Asset> = acvd_data::assets_in(m.set)
        .iter()
        .copied()
        .filter(|a| a.ext == ".ani")
        .collect();
    clips.sort_by_key(|a| a.entry);
    let index = clips
        .iter()
        .position(|a| a.entry == m.skeleton)
        .unwrap_or(0);
    let clip = Arc::new(skeleton.clone());
    let prefix = m.skeleton.split('_').next().unwrap_or_default();
    let current_anim_id = parse_anim_id(m.skeleton);
    let mut clip_by_id = HashMap::with_capacity(clips.len());
    for (i, a) in clips.iter().enumerate() {
        if let Some(id) = parse_prefixed_anim_id(prefix, a.entry) {
            clip_by_id.entry(id).or_insert(i);
        }
    }
    let mut cache = HashMap::new();
    cache.insert(index, Arc::clone(&clip));

    // Load legs TAE timeline (`tae/ac/motion_ow_disarm/own{:06}.tae`) from MotionCollateSt.
    let legs_motion_id = acvd_data::part_field(leg_part as i64, 3, "legs_motion_id") as u8;
    let collate = PARAM_MOTIONCOLLATE_BIN.iter().find(|r| {
        r.data.arms_motion_id == 0
            && r.data.legs_motion_id == legs_motion_id
            && r.data.ow_motion_id == 0
    });
    let sound_param_id = collate.map_or(0, |r| u32::from(r.data.sound_param_id));
    let tae_file = collate.and_then(|r| {
        let owner = r.data.motion_ow_disarm;
        let path = format!("tae/ac/motion_ow_disarm/own{owner:06}.tae");
        match vfs::open(disc, &path).and_then(|b| tae::read(&b)) {
            Ok(t) => {
                debug!(
                    "loaded TAE {path} ({} anims, legs_motion_id={legs_motion_id}, sound_param_id={sound_param_id})",
                    t.animations.len()
                );
                Some(t)
            }
            Err(e) => {
                warn!("TAE {path}: {e:#}");
                None
            }
        }
    });

    // Load optional `motion_ow_equip` bank + TAE (`0x8284e9d8`, `0x8284e7c8`) if the design
    // equips an Overed Weapon (`category == 11`).
    let ow_type = parts
        .iter()
        .find(|p| p.placement.category == 11)
        .map(|p| acvd_data::part_field(p.placement.part as i64, 11, "ow_type") as u8)
        .filter(|&t| t > 0);
    let ow_collate = ow_type.and_then(|ow_id| {
        PARAM_MOTIONCOLLATE_BIN.iter().find(|r| {
            r.data.arms_motion_id == 0
                && r.data.legs_motion_id == legs_motion_id
                && r.data.ow_motion_id == ow_id
        })
    });
    let (ow_equip_set, ow_equip_clips, ow_equip_tae) = match ow_collate {
        Some(r) => {
            let name = r.data.ow_equip_ac_motion_name;
            let binder = format!("model/ac/motion/motion_ow_equip/{name}/{name}_a.bnd.dcx");
            let mut ow_clips: Vec<&'static Asset> = acvd_data::assets_in(&binder)
                .iter()
                .copied()
                .filter(|a| a.ext == ".ani")
                .collect();
            ow_clips.sort_by_key(|a| a.entry);
            let ow_tae_path = format!("tae/ac/motion_ow_equip/own{:06}.tae", r.data.motion_ow_equip);
            let ow_tae = vfs::open(disc, &ow_tae_path)
                .and_then(|b| tae::read(&b))
                .ok();
            debug!(
                "loaded motion_ow_equip {binder} ({} clips, tae={})",
                ow_clips.len(),
                ow_tae.is_some()
            );
            (Some(binder), ow_clips, ow_tae)
        }
        None => (None, Vec::new(), None),
    };

    let mut ow_clip_by_id = HashMap::with_capacity(ow_equip_clips.len());
    for &a in &ow_equip_clips {
        if let Some(id) = parse_prefixed_anim_id(prefix, a.entry) {
            ow_clip_by_id.entry(id).or_insert(a);
        }
    }

    Ok(Some(Motion {
        set: m.set,
        skeleton,
        prefix,
        clips,
        index,
        clip,
        current_anim_id,
        frame: 0.0,
        playing: true,
        looping: true,
        speed: 1.0,
        wheel: None,
        lean: 0.0,
        in_place: false,
        scale: 1.0,
        fade: Fade::default(),
        upper: None,
        upper_return_fade: Fade::default(),
        arm_layers: [None, None],
        arm_return_fade: [Fade::default(), Fade::default()],
        core_ctrl: CoreControl::from_disc(disc),
        tae: tae_file,
        ow_equip_set,
        ow_equip_clips,
        ow_equip_tae,
        ow_bank_active: false,
        playing_ow_clip: false,
        sound_param_id,
        tae_events: Vec::new(),
        center_shift: Vec3::ZERO,
        center_ready: false,
        clip_by_id,
        ow_clip_by_id,
        cache,
        ow_cache: HashMap::new(),
    }))
}

pub fn build(disc: &Disc, parts: &[Loaded]) -> Result<Built> {
    let mut motion = motion(disc, parts)?;
    let rest: Vec<ani::Rest> = motion
        .as_ref()
        .map(|m| {
            m.skeleton
                .bones
                .iter()
                .filter_map(|b| b.rest.clone())
                .collect()
        })
        .unwrap_or_default();

    // Part bones first, one joint each.
    let mut joints: Vec<Spec> = Vec::new();
    let mut first = Vec::new();
    for p in parts {
        first.push(joints.len());
        joints.extend(p.rig.bones.iter().map(|b| Spec {
            parent: None,
            bind: affine(&b.bind),
            bone: None,
            virtual_bone: false,
        }));
    }
    let name = |j: usize| -> &str {
        let pi = first.iter().rposition(|&f| f <= j).unwrap_or(0);
        &parts[pi].rig.bones[j - first[pi]].name
    };
    // Weapon arms (`acvparts` cat-2 `+0x13c == 1`, 360 `0x824793c8`) animate all of their
    // non-root bones (`*_armwepunit_*`, `*_armroot`, `*_arm01..*_armhand`) via their own
    // `am*_a.bnd.dcx` clips (`0x8284cecc`, `0x82882d28`). Only their shoulder roots (`l_kata`,
    // `r_kata`, `parent.is_none()`) follow the AC body clip; letting the body clip overwrite
    // `*_arm01..*_armhand` twists them against `*_armwepunit_*` / `*_armroot` (e.g. `am7420`'s
    // 180-degree `r_armroot` bind rotation).
    let is_weapon_arm_child = |j: usize| -> bool {
        let pi = first.iter().rposition(|&f| f <= j).unwrap_or(0);
        let p = &parts[pi];
        p.placement.column == "arms"
            && acvd_data::part_field(p.placement.part as i64, 2, "is_weapon_arm") != 0.0
            && p.rig.bones[j - first[pi]].parent.is_some()
    };

    // Each clip bone claims the first part bone of its name, or becomes a joint of its own.
    let part_joints = joints.len();
    let mut of_bone: Vec<usize> = Vec::with_capacity(rest.len());
    for (i, r) in rest.iter().enumerate() {
        match (0..part_joints).find(|&j| {
            joints[j].bone.is_none() && !is_weapon_arm_child(j) && name(j) == r.name
        }) {
            Some(j) => {
                joints[j].bone = Some(i);
                of_bone.push(j);
            }
            None => {
                of_bone.push(joints.len());
                joints.push(Spec {
                    parent: None,
                    bind: Affine3A::IDENTITY,
                    bone: Some(i),
                    virtual_bone: true,
                });
            }
        }
    }

    // Parents: FLVER parents inside a part; clip parents for clip bones; socket carriers for
    // the remaining part roots.
    for (pi, p) in parts.iter().enumerate() {
        for (b, bone) in p.rig.bones.iter().enumerate() {
            let j = first[pi] + b;
            joints[j].parent = match bone.parent {
                Some(parent) => Some(first[pi] + parent),
                None => match joints[j].bone.and_then(|i| rest[i].parent) {
                    Some(cp) => of_bone.get(cp as usize).copied(),
                    None => carrier(parts, &first, p.placement, bone),
                },
            };
        }
    }
    for (i, r) in rest.iter().enumerate() {
        let j = of_bone[i];
        if joints[j].virtual_bone {
            joints[j].parent = r.parent.and_then(|cp| of_bone.get(cp as usize).copied());
        }
    }

    // Leg scale: the legs' bone lengths over the clip body's, below the legs' clip-driven root.
    if let Some(m) = motion.as_mut() {
        let (mut ours, mut theirs) = (0.0, 0.0);
        if let Some(pi) = parts
            .iter()
            .position(|p| acvd_data::ac_motion(p.placement.category, p.placement.part).is_some())
        {
            for b in 0..parts[pi].rig.bones.len() {
                let j = first[pi] + b;
                let (Some(i), Some(parent)) = (joints[j].bone, parts[pi].rig.bones[b].parent)
                else {
                    continue;
                };
                ours += (joints[first[pi] + parent].bind.inverse() * joints[j].bind)
                    .translation
                    .length();
                theirs += Vec3::from(rest[i].translation).length();
            }
        }
        m.scale = if theirs > 0.0 { ours / theirs } else { 1.0 };
    }

    // Clip-only joints rest where the clip's rest pose puts them.
    let scale = motion.as_ref().map_or(1.0, |m| m.scale);
    for (i, r) in rest.iter().enumerate() {
        let j = of_bone[i];
        if joints[j].virtual_bone {
            let parent = joints[j]
                .parent
                .map_or(Affine3A::IDENTITY, |p| joints[p].bind);
            let local = Affine3A::from_scale_rotation_translation(
                Vec3::from(r.scale),
                Quat::from_array(ani::euler_quat(r.euler)),
                Vec3::from(r.translation) * scale,
            );
            joints[j].bind = parent * local;
        }
    }

    let parts_out = parts
        .iter()
        .zip(&first)
        .map(|(p, &f)| (f, Affine3A::from_translation(Vec3::from(p.rig.unboned))))
        .collect();
    let groups = (0..rest.len()).map(|i| hokan_group(&rest, i)).collect();
    let upper = (0..rest.len()).map(|i| is_upper_body(&rest, i)).collect();
    let arm_side = (0..rest.len()).map(|i| arm_side_of(&rest, i)).collect();
    let core_bones = (0..rest.len()).map(|i| core_bone_of(&rest, i)).collect();
    Ok(Built {
        joints,
        parts: parts_out,
        groups,
        upper,
        arm_side,
        core_bones,
        motion,
    })
}

/// The joint carrying the socket `bone` (a part root) attaches to, if its slot has a parent.
fn carrier(
    parts: &[Loaded],
    first: &[usize],
    placement: &Placement,
    bone: &RigBone,
) -> Option<usize> {
    let mount = placement.mounts.iter().find(|m| m.root == bone.name)?;
    let (parent, socket) = (mount.parent, mount.socket);
    let p = parts.iter().position(|l| l.index == parent)?;
    let rig = &parts[p].rig;
    let b = match socket {
        SocketRef::Dummy(id) => rig.sockets.iter().find(|(s, _)| *s == id)?.1?,
        SocketRef::Bone(name) => rig.bones.iter().position(|x| x.name == name)?,
        SocketRef::None => return None,
    };
    Some(first[p] + b)
}

/// Spawns the joints under `ac`. The returned joint list is in [`Built`] order (one entity per
/// joint, parents included). Per loaded part: its skin, joint entities in bone order plus the
/// unboned joint, and their inverse bind poses (Bevy axes).
pub fn spawn(
    commands: &mut Commands,
    ac: Entity,
    built: &Built,
    parts: &[Loaded],
) -> (Vec<Entity>, Vec<(Vec<Entity>, Vec<Mat4>)>) {
    let entities: Vec<Entity> = built
        .joints
        .iter()
        .map(|_| {
            commands
                .spawn((Transform::default(), Visibility::default()))
                .id()
        })
        .collect();
    for (j, spec) in built.joints.iter().enumerate() {
        let parent_bind = spec
            .parent
            .map_or(Affine3A::IDENTITY, |p| built.joints[p].bind);
        let local = parent_bind.inverse() * spec.bind;
        let (s, r, t) = local.to_scale_rotation_translation();
        let mut e = commands.entity(entities[j]);
        let transform = to_transform(t, r, s);
        e.insert((transform, ChildOf(spec.parent.map_or(ac, |p| entities[p]))));
        if let Some(bone) = spec.bone {
            let absolute = spec.parent.is_none_or(|p| built.joints[p].virtual_bone);
            e.insert(Driven {
                bone,
                absolute,
                bind: t,
                upper: built.upper.get(bone).copied().unwrap_or(false),
                arm_side: built.arm_side.get(bone).copied().flatten(),
                core_bone: built.core_bones.get(bone).copied().unwrap_or_default(),
                group: built.groups.get(bone).copied().unwrap_or(0),
                base: transform,
                from: transform,
                generation: 0,
                upper_from: transform,
                upper_gen: 0,
                arm_from: transform,
                arm_gen: 0,
            });
        }
    }
    let skins = parts
        .iter()
        .zip(&built.parts)
        .map(|(p, &(f, unboned))| {
            let n = p.rig.bones.len();
            let extra = commands
                .spawn((
                    to_transform(unboned.translation.into(), Quat::IDENTITY, Vec3::ONE),
                    Visibility::default(),
                    ChildOf(ac),
                ))
                .id();
            let joints = entities[f..f + n].iter().copied().chain([extra]).collect();
            let binds = built.joints[f..f + n]
                .iter()
                .map(|s| s.bind)
                .chain([unboned])
                .map(|b| Mat4::from(mirror(b).inverse()))
                .collect();
            (joints, binds)
        })
        .collect();
    (entities, skins)
}

/// Per-entity AC animation driver for non-player ACs (such as AC TEST targets `a0000`–`a0003`),
/// independent of the player's global [`Motion`] resource.
#[derive(Component)]
pub struct NpcMotion {
    pub motion: Motion,
    joints: Vec<(Entity, Driven)>,
}

impl NpcMotion {
    /// Selects the test AC's locomotion clip (`acmotion.bin` / `motioncollate.bin`) from its
    /// local-space horizontal velocity (`+Z` backward, `-Z` forward, `+X` right in Bevy axes),
    /// and updates Core Control aim tracking toward the player AC.
    pub fn set_locomotion(
        &mut self,
        disc: &Disc,
        local_vel: Vec3,
        walk_speed: f32,
        aim_yaw_pitch: Option<(f32, f32)>,
    ) {
        let speed = Vec2::new(local_vel.x, local_vel.z).length();
        // Local forward is -Z in Bevy axes.
        let fwd = -local_vel.z;
        let right = local_vel.x;

        if speed < 1.2 {
            // Standing idle (`ac_states.csv` row 0 -> `acmotion.bin` anim_id 0).
            self.motion.wheel = None;
            let anim_id = find(PARAM_ACMOTION_BIN, 0).map_or(0, |r| r.data.anim_id);
            let _ = self.motion.play(disc, anim_id, true, 1.0, [0.15; 8]);
        } else if speed >= walk_speed.max(8.0) * 1.15 {
            // Dash / boost strafe (`ac_states.csv` row 17 -> 360-frame wheel `anim_id 55`).
            let deg = right.atan2(fwd).to_degrees().rem_euclid(360.0);
            let dash_id = find(PARAM_ACMOTION_BIN, 17).map_or(55, |r| r.data.anim_id);
            if self.motion.play(disc, dash_id, false, 1.0, [0.12; 8]).is_ok() {
                self.motion.wheel = Some(deg);
                self.motion.lean = 1.0;
            } else {
                self.motion.wheel = None;
                let walk_id = find(PARAM_ACMOTION_BIN, 9).map_or(10, |r| r.data.anim_id);
                let _ = self.motion.play(disc, walk_id, true, 1.25, [0.12; 8]);
            }
        } else {
            // 8-direction walk (`ac_states.csv` rows 9..=16, clockwise from forward).
            self.motion.wheel = None;
            let deg = right.atan2(fwd).to_degrees().rem_euclid(360.0);
            let octant = ((deg + 22.5) / 45.0).floor() as u32 % 8;
            let row_id = 9 + octant;
            let anim_id = find(PARAM_ACMOTION_BIN, row_id).map_or(10, |r| r.data.anim_id);
            let play_speed = (speed / walk_speed.max(6.0)).clamp(0.7, 1.4);
            if self.motion.play(disc, anim_id, true, play_speed, [0.12; 8]).is_err() {
                let fallback = find(PARAM_ACMOTION_BIN, 9).map_or(10, |r| r.data.anim_id);
                let _ = self.motion.play(disc, fallback, true, play_speed, [0.12; 8]);
            }
        }

        if let Some((yaw, pitch)) = aim_yaw_pitch {
            self.motion.core_ctrl.enabled = true;
            self.motion.core_ctrl.target_yaw = yaw;
            self.motion.core_ctrl.target_pitch = pitch;
        } else {
            self.motion.core_ctrl.enabled = false;
        }
    }

    /// Advances the clip clock and Core Control by `dt` seconds and returns the current frame.
    pub fn step_clock(&mut self, dt: f32) -> f32 {
        let m = &mut self.motion;
        let frames = m.clip.frames.max(1) as f32;
        if let Some(target) = m.wheel {
            let diff = (target - m.frame + 540.0).rem_euclid(360.0) - 180.0;
            let step = WHEEL_RATE * dt;
            m.frame = (m.frame + diff.clamp(-step, step)).rem_euclid(360.0);
        } else if m.playing {
            let next = m.frame + dt * FRAME_RATE * m.speed;
            m.frame = if m.looping {
                next % frames
            } else {
                next.min(frames - 1.0)
            };
        }
        m.fade.t += dt;
        m.core_ctrl.step(dt);
        m.frame
    }
}

/// Spawns the joints under `ac` with a per-entity [`NpcMotion`] component so test ACs
/// (`a0000`–`a0003`) play full locomotion clips and Core Control aim tracking independently of
/// the player's [`Motion`] resource.
pub fn spawn_npc(
    commands: &mut Commands,
    ac: Entity,
    built: Built,
    parts: &[Loaded],
) -> (Option<NpcMotion>, Vec<Entity>, Vec<(Vec<Entity>, Vec<Mat4>)>) {
    let entities: Vec<Entity> = built
        .joints
        .iter()
        .map(|_| {
            commands
                .spawn((Transform::default(), Visibility::default()))
                .id()
        })
        .collect();
    let mut driven_joints = Vec::new();
    for (j, spec) in built.joints.iter().enumerate() {
        let parent_bind = spec
            .parent
            .map_or(Affine3A::IDENTITY, |p| built.joints[p].bind);
        let local = parent_bind.inverse() * spec.bind;
        let (s, r, t) = local.to_scale_rotation_translation();
        let transform = to_transform(t, r, s);
        commands
            .entity(entities[j])
            .insert((transform, ChildOf(spec.parent.map_or(ac, |p| entities[p]))));
        if let Some(bone) = spec.bone {
            let absolute = spec.parent.is_none_or(|p| built.joints[p].virtual_bone);
            driven_joints.push((
                entities[j],
                Driven {
                    bone,
                    absolute,
                    bind: t,
                    upper: built.upper.get(bone).copied().unwrap_or(false),
                    arm_side: built.arm_side.get(bone).copied().flatten(),
                    core_bone: built.core_bones.get(bone).copied().unwrap_or_default(),
                    group: built.groups.get(bone).copied().unwrap_or(0),
                    base: transform,
                    from: transform,
                    generation: 0,
                    upper_from: transform,
                    upper_gen: 0,
                    arm_from: transform,
                    arm_gen: 0,
                },
            ));
        }
    }
    let skins = parts
        .iter()
        .zip(&built.parts)
        .map(|(p, &(f, unboned))| {
            let n = p.rig.bones.len();
            let extra = commands
                .spawn((
                    to_transform(unboned.translation.into(), Quat::IDENTITY, Vec3::ONE),
                    Visibility::default(),
                    ChildOf(ac),
                ))
                .id();
            let joints = entities[f..f + n].iter().copied().chain([extra]).collect();
            let binds = built.joints[f..f + n]
                .iter()
                .map(|s| s.bind)
                .chain([unboned])
                .map(|b| Mat4::from(mirror(b).inverse()))
                .collect();
            (joints, binds)
        })
        .collect();
    let npc = built.motion.map(|motion| NpcMotion {
        motion,
        joints: driven_joints,
    });
    (npc, entities, skins)
}

/// Steps all [`NpcMotion`] test-AC skeletons and writes their posed joint transforms.
pub fn animate_npcs(
    time: Res<Time>,
    mut npcs: Query<&mut NpcMotion>,
    mut transforms: Query<&mut Transform>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    for mut npc in &mut npcs {
        npc.step_clock(dt);
        let NpcMotion { motion: m, joints } = &mut *npc;
        let (f, scale, fade) = (m.frame, m.scale, m.fade);
        let wheel = m.wheel.is_some();
        let lean = m.lean;
        let cc = m.core_ctrl;

        for (entity, d) in joints.iter_mut() {
            let Ok(mut transform) = transforms.get_mut(*entity) else {
                continue;
            };
            let Some(rest) = m.skeleton.bones.get(d.bone).and_then(|b| b.rest.as_ref()) else {
                continue;
            };
            if d.generation != fade.generation {
                (d.from, d.generation) = (d.base, fade.generation);
            }
            let Some(bone) = m.clip.bones.get(d.bone) else {
                continue;
            };
            let bone = if m.in_place && rest.parent.is_none() {
                &m.skeleton.bones[d.bone]
            } else {
                bone
            };
            let rotation = Quat::from_array(
                bone.track
                    .rotation(f)
                    .unwrap_or_else(|| ani::euler_quat(rest.euler)),
            );
            let t = Vec3::from(bone.track.translation(f).unwrap_or(rest.translation));
            let s = Vec3::from(bone.track.scale(f).unwrap_or(rest.scale));
            let translation = if d.absolute {
                t * scale
            } else {
                d.bind + (t - Vec3::from(rest.translation)) * scale
            };
            let target = to_transform(translation, rotation, s);
            let secs = fade.secs[d.group];
            let w = if wheel {
                lean
            } else if secs > 0.0 {
                (fade.t / secs).min(1.0)
            } else {
                1.0
            };
            d.base = if w >= 1.0 {
                target
            } else {
                Transform {
                    translation: d.from.translation.lerp(target.translation, w),
                    rotation: d.from.rotation.slerp(target.rotation, w),
                    scale: d.from.scale.lerp(target.scale, w),
                }
            };
            *transform = d.base;
            if cc.weight > 1e-4 {
                match d.core_bone {
                    CoreBone::Core => {
                        let yaw_q = Quat::from_rotation_y(cc.yaw * cc.weight);
                        let pitch_q = Quat::from_rotation_x(cc.pitch * cc.weight);
                        transform.rotation = yaw_q * pitch_q * transform.rotation;
                    }
                    CoreBone::Head => {
                        let yaw_q = Quat::from_rotation_y(cc.yaw * cc.weight * 0.2);
                        let pitch_q = Quat::from_rotation_x(cc.pitch * cc.weight * 0.25);
                        transform.rotation = yaw_q * pitch_q * transform.rotation;
                    }
                    CoreBone::None => {}
                }
            }
        }
    }
}

/// Spawns the joints under `ac` posed on frame 0 of `built.motion` without inserting [`Driven`],
/// so non-player ACs (such as AC TEST targets) hold their standing pose independently of the
/// player's [`Motion`] resource.
#[allow(dead_code)]
pub fn spawn_static(
    commands: &mut Commands,
    ac: Entity,
    built: &Built,
    parts: &[Loaded],
) -> (Vec<Entity>, Vec<(Vec<Entity>, Vec<Mat4>)>) {
    let entities: Vec<Entity> = built
        .joints
        .iter()
        .map(|_| {
            commands
                .spawn((Transform::default(), Visibility::default()))
                .id()
        })
        .collect();
    for (j, spec) in built.joints.iter().enumerate() {
        let parent_bind = spec
            .parent
            .map_or(Affine3A::IDENTITY, |p| built.joints[p].bind);
        let local = parent_bind.inverse() * spec.bind;
        let (s, r, t) = local.to_scale_rotation_translation();
        let mut transform = to_transform(t, r, s);
        if let (Some(bone), Some(m)) = (spec.bone, built.motion.as_ref()) {
            if let (Some(rest), Some(cbone)) = (
                m.skeleton.bones.get(bone).and_then(|b| b.rest.as_ref()),
                m.clip.bones.get(bone),
            ) {
                let cbone = if m.in_place && rest.parent.is_none() {
                    &m.skeleton.bones[bone]
                } else {
                    cbone
                };
                let rotation = Quat::from_array(
                    cbone
                        .track
                        .rotation(0.0)
                        .unwrap_or_else(|| ani::euler_quat(rest.euler)),
                );
                let ct = Vec3::from(cbone.track.translation(0.0).unwrap_or(rest.translation));
                let cs = Vec3::from(cbone.track.scale(0.0).unwrap_or(rest.scale));
                let absolute = spec.parent.is_none_or(|p| built.joints[p].virtual_bone);
                let translation = if absolute {
                    ct * m.scale
                } else {
                    t + (ct - Vec3::from(rest.translation)) * m.scale
                };
                transform = to_transform(translation, rotation, cs);
            }
        }
        commands
            .entity(entities[j])
            .insert((transform, ChildOf(spec.parent.map_or(ac, |p| entities[p]))));
    }
    let skins = parts
        .iter()
        .zip(&built.parts)
        .map(|(p, &(f, unboned))| {
            let n = p.rig.bones.len();
            let extra = commands
                .spawn((
                    to_transform(unboned.translation.into(), Quat::IDENTITY, Vec3::ONE),
                    Visibility::default(),
                    ChildOf(ac),
                ))
                .id();
            let joints = entities[f..f + n].iter().copied().chain([extra]).collect();
            let binds = built.joints[f..f + n]
                .iter()
                .map(|s| s.bind)
                .chain([unboned])
                .map(|b| Mat4::from(mirror(b).inverse()))
                .collect();
            (joints, binds)
        })
        .collect();
    (entities, skins)
}

/// Advances the clip(s), fires crossed TAE events, steps Core Control, and poses every driven
/// joint (with upper/lower body layering and crossfading).
pub fn animate(
    time: Res<Time>,
    motion: Option<ResMut<Motion>>,
    mut joints: Query<(&mut Driven, &mut Transform)>,
) {
    let Some(mut m) = motion else { return };
    let dt = time.delta_secs().min(0.1);
    m.tae_events.clear();
    let frames = m.clip.frames.max(1) as f32;
    if let Some(target) = m.wheel {
        let diff = (target - m.frame + 540.0).rem_euclid(360.0) - 180.0;
        let step = WHEEL_RATE * dt;
        m.frame = (m.frame + diff.clamp(-step, step)).rem_euclid(360.0);
        // The wheel's `center` translation is the body's shift into the heading. Follow it
        // slowly so a strafe change leans the limbs without shoving the whole mech.
        let center = m
            .skeleton
            .bones
            .iter()
            .position(|b| b.rest.as_ref().is_some_and(|r| r.name == "center"));
        if let Some(goal) = center
            .and_then(|i| m.clip.bones.get(i))
            .and_then(|b| b.track.translation(m.frame))
        {
            let goal = Vec3::from(goal);
            if !m.center_ready {
                m.center_shift = goal;
                m.center_ready = true;
            } else {
                let step = CENTER_SHIFT_RATE * dt;
                let d = goal - m.center_shift;
                let len = d.length();
                m.center_shift = if len > step {
                    m.center_shift + d * (step / len)
                } else {
                    goal
                };
            }
        }
    } else {
        m.center_ready = false;
        if m.playing {
            let prev = m.frame;
            let next = prev + dt * FRAME_RATE * m.speed;
            m.frame = if m.looping {
                next % frames
            } else {
                next.min(frames - 1.0)
            };
            let curr = m.frame;
            let anim_id = u32::from(m.current_anim_id);
            let looping = m.looping;
            let active_tae = if m.playing_ow_clip {
                m.ow_equip_tae.as_ref().or(m.tae.as_ref())
            } else {
                m.tae.as_ref()
            };
            if let Some(anim) = active_tae.and_then(|t| t.animation(anim_id)) {
                let fired = crossed_events(anim, prev, curr, frames, looping);
                m.tae_events.extend(fired);
            }
        }
    }
    m.fade.t += dt;
    m.upper_return_fade.t += dt;
    m.arm_return_fade[0].t += dt;
    m.arm_return_fade[1].t += dt;

    // Advance upper-body layer and fire its TAE events if active.
    let upper_step = if let Some(ref mut u) = m.upper {
        let u_frames = u.clip.frames.max(1) as f32;
        let u_prev = u.frame;
        let u_next = u_prev + dt * FRAME_RATE * u.speed;
        u.frame = if u.looping {
            u_next % u_frames
        } else {
            u_next.min(u_frames - 1.0)
        };
        u.fade.t += dt;
        Some((u32::from(u.anim_id), u_prev, u.frame, u_frames, u.looping))
    } else {
        None
    };
    if let Some((u_id, u_prev, u_curr, u_frames, u_looping)) = upper_step {
        let upper_tae = if m.ow_bank_active {
            m.ow_equip_tae.as_ref().or(m.tae.as_ref())
        } else {
            m.tae.as_ref()
        };
        if let Some(anim) = upper_tae.and_then(|t| t.animation(u_id)) {
            let fired = crossed_events(anim, u_prev, u_curr, u_frames, u_looping);
            m.tae_events.extend(fired);
        }
    }

    // Advance per-arm overlay layers (right `r_kata` / left `l_kata`, 360 layers 0x0a / 0x0b)
    // and fire their TAE timelines (`2000..2003` bay shift, `2010..2013` bay purge).
    let mut arm_steps = [None, None];
    for side in 0..2 {
        if let Some(ref mut a) = m.arm_layers[side] {
            let a_frames = a.clip.frames.max(1) as f32;
            let a_prev = a.frame;
            let a_next = a_prev + dt * FRAME_RATE * a.speed;
            // Clamp at `a_frames` (not `a_frames - 1`) so frame-150 TAE grab events fire on 150-frame clips.
            a.frame = a_next.min(a_frames);
            a.fade.t += dt;
            arm_steps[side] = Some((a.tae_id, a_prev, a.frame, a_frames));
        }
    }
    for step in arm_steps.into_iter().flatten() {
        let (tae_id, a_prev, a_curr, a_frames) = step;
        if let Some(anim) = m.tae.as_ref().and_then(|t| t.animation(tae_id)) {
            // 360 bay-shift TAE FFX (`4109`, `98`) spawn at scale 1.5 (Xenia `bay_switch.jsonl`).
            let fired = crossed_events_scaled(anim, a_prev, a_curr, a_frames, false, 1.5);
            m.tae_events.extend(fired);
        }
    }

    // Step Core Control procedural aim tracking.
    m.core_ctrl.step(dt);

    let (f, scale, fade, return_fade) = (m.frame, m.scale, m.fade, m.upper_return_fade);
    let arm_return = m.arm_return_fade;
    let (wheel, lean) = (m.wheel.is_some(), m.lean);
    let cc = m.core_ctrl;
    for (mut d, mut transform) in &mut joints {
        let Some(rest) = m.skeleton.bones.get(d.bone).and_then(|b| b.rest.as_ref()) else {
            continue;
        };

        let arm_pose = d.arm_side.and_then(|side| {
            m.arm_layers[usize::from(side)]
                .as_ref()
                .and_then(|a| a.clip.bones.get(d.bone).map(|b| (a, b)))
        });
        if let Some((a, abone)) = arm_pose {
            if d.arm_gen != a.fade.generation {
                (d.arm_from, d.arm_gen) = (d.base, a.fade.generation);
            }
            let sample_frame = a.frame.min(a.clip.frames.saturating_sub(1) as f32);
            let rotation = Quat::from_array(
                abone
                    .track
                    .rotation(sample_frame)
                    .unwrap_or_else(|| ani::euler_quat(rest.euler)),
            );
            let t = Vec3::from(
                abone
                    .track
                    .translation(sample_frame)
                    .unwrap_or(rest.translation),
            );
            let s = Vec3::from(abone.track.scale(sample_frame).unwrap_or(rest.scale));
            let translation = if d.absolute {
                t * scale
            } else {
                d.bind + (t - Vec3::from(rest.translation)) * scale
            };
            let target = to_transform(translation, rotation, s);
            let secs = a.fade.secs[d.group];
            let w = if secs > 0.0 {
                (a.fade.t / secs).min(1.0)
            } else {
                1.0
            };
            d.base = if w >= 1.0 {
                target
            } else {
                Transform {
                    translation: d.arm_from.translation.lerp(target.translation, w),
                    rotation: d.arm_from.rotation.slerp(target.rotation, w),
                    scale: d.arm_from.scale.lerp(target.scale, w),
                }
            };
            *transform = d.base;
            continue;
        }

        let upper_pose = if d.upper {
            m.upper
                .as_ref()
                .and_then(|u| u.clip.bones.get(d.bone).map(|b| (u, b)))
        } else {
            None
        };
        if let Some((u, ubone)) = upper_pose {
            if d.upper_gen != u.fade.generation {
                (d.upper_from, d.upper_gen) = (d.base, u.fade.generation);
            }
            let rotation = Quat::from_array(
                ubone
                    .track
                    .rotation(u.frame)
                    .unwrap_or_else(|| ani::euler_quat(rest.euler)),
            );
            let t = Vec3::from(ubone.track.translation(u.frame).unwrap_or(rest.translation));
            let s = Vec3::from(ubone.track.scale(u.frame).unwrap_or(rest.scale));
            let translation = if d.absolute {
                t * scale
            } else {
                d.bind + (t - Vec3::from(rest.translation)) * scale
            };
            let target = to_transform(translation, rotation, s);
            let secs = u.fade.secs[d.group];
            let w = if secs > 0.0 {
                (u.fade.t / secs).min(1.0)
            } else {
                1.0
            };
            d.base = if w >= 1.0 {
                target
            } else {
                Transform {
                    translation: d.upper_from.translation.lerp(target.translation, w),
                    rotation: d.upper_from.rotation.slerp(target.rotation, w),
                    scale: d.upper_from.scale.lerp(target.scale, w),
                }
            };
            *transform = d.base;
        } else {
            if d.generation != fade.generation {
                (d.from, d.generation) = (d.base, fade.generation);
            }
            let returning_upper = d.upper && return_fade.generation != 0 && d.upper_gen != return_fade.generation;
            if returning_upper {
                (d.upper_from, d.upper_gen) = (d.base, return_fade.generation);
            }
            let Some(bone) = m.clip.bones.get(d.bone) else {
                continue;
            };
            let bone = if m.in_place && rest.parent.is_none() {
                &m.skeleton.bones[d.bone]
            } else {
                bone
            };
            let rotation = Quat::from_array(
                bone.track
                    .rotation(f)
                    .unwrap_or_else(|| ani::euler_quat(rest.euler)),
            );
            let t = if wheel && m.center_ready && rest.name == "center" {
                m.center_shift
            } else {
                Vec3::from(bone.track.translation(f).unwrap_or(rest.translation))
            };
            let s = Vec3::from(bone.track.scale(f).unwrap_or(rest.scale));
            let translation = if d.absolute {
                t * scale
            } else {
                d.bind + (t - Vec3::from(rest.translation)) * scale
            };
            let target = to_transform(translation, rotation, s);
            let secs = fade.secs[d.group];
            let w = if wheel {
                lean
            } else if secs > 0.0 {
                (fade.t / secs).min(1.0)
            } else {
                1.0
            };
            let base = if w >= 1.0 {
                target
            } else {
                Transform {
                    translation: d.from.translation.lerp(target.translation, w),
                    rotation: d.from.rotation.slerp(target.rotation, w),
                    scale: d.from.scale.lerp(target.scale, w),
                }
            };
            // Smooth crossfade back from a recently cleared upper-body clip.
            let ret_secs = return_fade.secs[d.group];
            d.base = if d.upper && return_fade.generation != 0 && ret_secs > 0.0 && return_fade.t < ret_secs {
                let rw = (return_fade.t / ret_secs).clamp(0.0, 1.0);
                Transform {
                    translation: d.upper_from.translation.lerp(base.translation, rw),
                    rotation: d.upper_from.rotation.slerp(base.rotation, rw),
                    scale: d.upper_from.scale.lerp(base.scale, rw),
                }
            } else {
                base
            };
            // Smooth crossfade back from a recently cleared per-arm overlay (bay shift/purge).
            if let Some(side) = d.arm_side {
                let af = arm_return[usize::from(side)];
                if af.generation != 0 && d.arm_gen != af.generation {
                    (d.arm_from, d.arm_gen) = (d.base, af.generation);
                }
                let a_ret = af.secs[d.group];
                if af.generation != 0 && a_ret > 0.0 && af.t < a_ret {
                    let aw = (af.t / a_ret).clamp(0.0, 1.0);
                    d.base = Transform {
                        translation: d.arm_from.translation.lerp(d.base.translation, aw),
                        rotation: d.arm_from.rotation.slerp(d.base.rotation, aw),
                        scale: d.arm_from.scale.lerp(d.base.scale, aw),
                    };
                }
            }
            *transform = d.base;
        }

        // Apply Core Control rotation to `core` (and a subtle lead to `head_01`).
        if cc.weight > 1e-4 {
            match d.core_bone {
                CoreBone::Core => {
                    let yaw_q = Quat::from_rotation_y(cc.yaw * cc.weight);
                    let pitch_q = Quat::from_rotation_x(cc.pitch * cc.weight);
                    transform.rotation = yaw_q * pitch_q * transform.rotation;
                }
                CoreBone::Head => {
                    let yaw_q = Quat::from_rotation_y(cc.yaw * cc.weight * 0.2);
                    let pitch_q = Quat::from_rotation_x(cc.pitch * cc.weight * 0.25);
                    transform.rotation = yaw_q * pitch_q * transform.rotation;
                }
                CoreBone::None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hokan_groups_follow_the_nearest_named_ancestor() {
        let names = [
            ("master", None),
            ("center", Some(0)),
            ("c_center", Some(1)),
            ("core", Some(2)),
            ("l_kata", Some(3)),
            ("k_center", Some(1)),
            ("kosi", Some(5)),
            ("l_momo_01", Some(6)),
            ("l_hiza", Some(7)),
            ("r_sune", Some(8)),
            ("r_asik_01", Some(9)),
        ];
        let rest: Vec<ani::Rest> = names
            .iter()
            .map(|&(name, parent)| ani::Rest {
                name: name.into(),
                kind: 0,
                parent,
                translation: [0.0; 3],
                euler: [0.0; 3],
                scale: [1.0; 3],
            })
            .collect();
        let groups: Vec<usize> = (0..rest.len()).map(|i| hokan_group(&rest, i)).collect();
        assert_eq!(groups, [0, 1, 2, 4, 4, 3, 5, 6, 6, 7, 7]);
    }

    #[test]
    fn fade_converts_hokan_frames_to_seconds() {
        let walk = find_hokan(2);
        assert_eq!(Fade::of(&walk)[0], 0.06);
        assert_eq!(Fade::of(&walk)[3], 0.3);
        assert_eq!(Fade::booster(&walk), 0.06);
        assert_eq!(Fade::booster(&find_hokan(4)), 0.015);
    }

    fn find_hokan(id: u32) -> AcanimHokanparamSt {
        acvd_data::find(acvd_data::generated::ac_unit::PARAM_ACANIMHOKAN_BIN, id)
            .expect("hokan row")
            .data
    }

    #[test]
    fn upper_body_split_separates_core_subtree_from_legs() {
        let names = [
            ("master", None),
            ("center", Some(0)),
            ("c_center", Some(1)),
            ("core", Some(2)),
            ("head_01", Some(3)),
            ("l_kata", Some(3)),
            ("k_center", Some(1)),
            ("kosi", Some(6)),
            ("l_momo_01", Some(7)),
            ("r_asik_01", Some(8)),
        ];
        let rest: Vec<ani::Rest> = names
            .iter()
            .map(|&(name, parent)| ani::Rest {
                name: name.into(),
                kind: 0,
                parent,
                translation: [0.0; 3],
                euler: [0.0; 3],
                scale: [1.0; 3],
            })
            .collect();
        let upper: Vec<bool> = (0..rest.len()).map(|i| is_upper_body(&rest, i)).collect();
        assert_eq!(
            upper,
            [false, false, true, true, true, true, false, false, false, false]
        );
        assert_eq!(core_bone_of(&rest, 3), CoreBone::Core);
        assert_eq!(core_bone_of(&rest, 4), CoreBone::Head);
        assert_eq!(core_bone_of(&rest, 7), CoreBone::None);
    }

    #[test]
    fn core_control_tracks_clamps_and_respects_disable() {
        let mut cc = CoreControl::default();
        // Command an out-of-range yaw (+150 deg) and pitch (+45 deg).
        cc.target_yaw = 150.0_f32.to_radians();
        cc.target_pitch = 45.0_f32.to_radians();
        // After 0.25s at 180 deg/s, yaw advances 45 deg and pitch hits its +10 deg clamp.
        cc.step(0.25);
        assert!((cc.yaw.to_degrees() - 45.0).abs() < 1e-3);
        assert!((cc.pitch.to_degrees() - 10.0).abs() < 1e-3);
        // After another 1.0s, yaw clamps at +90 deg.
        cc.step(1.0);
        assert!((cc.yaw.to_degrees() - 90.0).abs() < 1e-3);
        // Disabling (`b_disable_core_ctrl == 1`) eases weight down and returns angles to 0.
        cc.enabled = false;
        cc.step(1.0);
        assert!(cc.weight < 0.6 && cc.weight > 0.4);
        assert!(cc.yaw.abs() < 1e-3);
        cc.step(1.5);
        assert_eq!(cc.weight, 0.0);
    }

    #[test]
    fn crossed_events_fires_on_advance_and_loop_wrap() {
        let anim = tae::Animation {
            id: 2,
            events: vec![
                tae::Event {
                    start_frame: 19,
                    end_frame: 22,
                    kind: tae::KIND_FOOTSTEP,
                    params: [1u32.to_be_bytes(), 1u32.to_be_bytes()].concat(),
                },
                tae::Event {
                    start_frame: 44,
                    end_frame: 47,
                    kind: tae::KIND_FOOTSTEP,
                    params: [0u32.to_be_bytes(), 1u32.to_be_bytes()].concat(),
                },
                tae::Event {
                    start_frame: 2,
                    end_frame: 4,
                    kind: tae::KIND_SOUND,
                    params: [1u32.to_be_bytes(), 89u32.to_be_bytes()].concat(),
                },
            ],
        };
        // Normal advance crossing frame 19.
        let hit = crossed_events(&anim, 18.5, 19.5, 50.0, true);
        assert_eq!(hit, vec![TaeFired::Footstep { side: 1, kind: 1 }]);
        // No events between 20 and 40.
        assert!(crossed_events(&anim, 20.0, 40.0, 50.0, true).is_empty());
        // Loop wrap from 48.0 -> 3.0 crosses frame 2.
        let wrapped = crossed_events(&anim, 48.0, 3.0, 50.0, true);
        assert_eq!(
            wrapped,
            vec![TaeFired::Sound {
                cue: "c00000089".into()
            }]
        );
    }

    #[test]
    fn ow_design_loads_motion_ow_equip_bank_and_reattaches_root_60() {
        use crate::assemble;
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        use acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS;
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        // Pick a preset design with an OW that has secondary root `"60"` (`ow0110`/`ow0310`/`ow0610`).
        let design = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| {
                r.data.back_r > 0
                    && assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, &r.data)
                        .placements
                        .iter()
                        .any(|p| p.column == "back_r" && p.model.root("60").is_some())
            })
            .expect("preset OW design with root 60");
        let built = assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, &design.data);
        let mut loaded = Vec::new();
        let mut known = vec![None; built.placements.len()];
        for (index, p) in built.placements.iter().enumerate() {
            let places = assemble::root_places(p, &built.placements, &known);
            let (meshes, rig) = acvd_render::rigged_model(&disc, p.model.path, &|root| {
                assemble::place_of(&places, root)
            })
            .expect("rigged_model");
            known[index] = Some(assemble::Stored {
                places,
                frames: rig.frames.clone(),
            });
            loaded.push(Loaded {
                index,
                placement: p,
                meshes,
                rig,
            });
        }
        let mut rig = build(&disc, &loaded).expect("pose::build");
        let motion = rig.motion.as_mut().expect("motion");
        assert!(
            motion
                .ow_equip_set
                .as_deref()
                .is_some_and(|s| s.contains("motion_ow_equip")),
            "ow_equip_set={:?}",
            motion.ow_equip_set
        );
        assert!(motion.has_ow_clip(200), "should have OW arm clip 200");
        assert!(motion.has_ow_clip(201), "should have OW armed idle clip 201");
        assert!(motion.ow_equip_tae.is_some(), "should load motion_ow_equip TAE");

        // Switch bank and play OW armed idle clip 201.
        motion.set_ow_bank(true);
        assert!(motion.ow_bank_active);
        motion
            .play(&disc, 201, true, 1.0, [0.0; 8])
            .expect("play OW clip 201");
        assert!(motion.playing_ow_clip);
        assert_eq!(motion.current_anim_id, 201);
        assert!(motion.name().contains("201"), "name={}", motion.name());

        // Verify OW root "60" -> arm dummy 60 re-attachment transform computes cleanly.
        let arms_li = loaded
            .iter()
            .position(|l| l.placement.column == "arms")
            .expect("arms");
        let ow_li = loaded
            .iter()
            .position(|l| l.placement.column == "back_r")
            .expect("back_r");
        let arm_bone = loaded[arms_li]
            .rig
            .sockets
            .iter()
            .find(|&&(id, _)| id == 60)
            .and_then(|&(_, b)| b)
            .expect("arm dummy 60 bone");
        let b60 = loaded[ow_li]
            .rig
            .bones
            .iter()
            .position(|b| b.name == "60" && b.parent.is_none())
            .expect("ow root 60");
        let stowed = assemble::place_of(
            &known[loaded[ow_li].index].as_ref().unwrap().places,
            Some("60"),
        );
        let deployed = assemble::ow_deployed_place(
            loaded[ow_li].placement,
            loaded[arms_li].placement,
            known[loaded[arms_li].index].as_ref().unwrap(),
        )
        .expect("ow_deployed_place");
        let local = rig
            .local_for_reattach(ow_li, b60, stowed, deployed, arms_li, arm_bone)
            .expect("local_for_reattach");
        assert!(local.translation.is_finite() && local.rotation.is_finite());
    }

    #[test]
    fn weapon_arm_child_bones_are_not_driven_by_body_clip() {
        use crate::assemble;
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        use acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS;
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        // Design 6009 uses weapon arms 7420 (`am7420.flv`), whose `r_armroot` has a 180-deg bind
        // rotation and whose `*_arm01..*_armhand` are driven by `am7420_a.bnd.dcx`, not `a01_000`.
        let design = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 6009)
            .expect("design 6009");
        let built = assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, &design.data);
        let mut loaded = Vec::new();
        let mut known = vec![None; built.placements.len()];
        for (index, p) in built.placements.iter().enumerate() {
            let places = assemble::root_places(p, &built.placements, &known);
            let (meshes, rig) = acvd_render::rigged_model(&disc, p.model.path, &|root| {
                assemble::place_of(&places, root)
            })
            .expect("rigged_model");
            known[index] = Some(assemble::Stored {
                places,
                frames: rig.frames.clone(),
            });
            loaded.push(Loaded {
                index,
                placement: p,
                meshes,
                rig,
            });
        }
        let rig = build(&disc, &loaded).expect("pose::build");
        let arms_li = loaded
            .iter()
            .position(|l| l.placement.column == "arms")
            .expect("arms");
        let first = rig.parts[arms_li].0;
        let mut roots_driven = 0usize;
        for (b, bone) in loaded[arms_li].rig.bones.iter().enumerate() {
            let spec = &rig.joints[first + b];
            if bone.parent.is_none() && (bone.name == "l_kata" || bone.name == "r_kata") {
                assert!(
                    spec.bone.is_some(),
                    "weapon arm shoulder root {} must stay driven by body clip",
                    bone.name
                );
                roots_driven += 1;
            } else if bone.parent.is_some() {
                assert!(
                    spec.bone.is_none(),
                    "weapon arm child bone {} must not be claimed by body clip",
                    bone.name
                );
            }
        }
        assert_eq!(roots_driven, 2, "expected both l_kata and r_kata roots driven");
    }

    #[test]
    fn core_control_does_not_compound_across_clip_transitions() {
        use crate::assemble;
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        use acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS;
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        let design = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 6007)
            .expect("design 6007");
        let built = assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, &design.data);
        let mut loaded = Vec::new();
        let mut known = vec![None; built.placements.len()];
        for (index, p) in built.placements.iter().enumerate() {
            let places = assemble::root_places(p, &built.placements, &known);
            let (meshes, rig) = acvd_render::rigged_model(&disc, p.model.path, &|root| {
                assemble::place_of(&places, root)
            })
            .expect("rigged_model");
            known[index] = Some(assemble::Stored {
                places,
                frames: rig.frames.clone(),
            });
            loaded.push(Loaded {
                index,
                placement: p,
                meshes,
                rig,
            });
        }
        let rig = build(&disc, &loaded).expect("pose::build");
        let mut motion = rig.motion.expect("motion");
        let core_bone = motion
            .skeleton
            .bones
            .iter()
            .position(|b| b.rest.as_ref().is_some_and(|r| r.name == "core"))
            .expect("core bone");
        let rest_q = {
            let r = motion.skeleton.bones[core_bone].rest.as_ref().unwrap();
            to_transform(
                Vec3::from(r.translation),
                Quat::from_array(ani::euler_quat(r.euler)),
                Vec3::from(r.scale),
            )
            .rotation
        };

        motion.core_ctrl.yaw = 45.0_f32.to_radians();
        motion.core_ctrl.pitch = -10.0_f32.to_radians();
        motion.core_ctrl.target_yaw = motion.core_ctrl.yaw;
        motion.core_ctrl.target_pitch = motion.core_ctrl.pitch;
        motion.core_ctrl.weight = 1.0;
        motion.fade.secs = [0.2; 8];

        let mut world = World::new();
        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
        world.insert_resource(time);
        world.insert_resource(motion);
        let joint = world
            .spawn((
                Transform::from_rotation(rest_q),
                Driven {
                    bone: core_bone,
                    absolute: false,
                    bind: Vec3::ZERO,
                    upper: true,
                    arm_side: None,
                    core_bone: CoreBone::Core,
                    group: 0,
                    base: Transform::from_rotation(rest_q),
                    from: Transform::from_rotation(rest_q),
                    generation: 0,
                    upper_from: Transform::from_rotation(rest_q),
                    upper_gen: 0,
                    arm_from: Transform::from_rotation(rest_q),
                    arm_gen: 0,
                },
            ))
            .id();

        let mut schedule = Schedule::default();
        schedule.add_systems(animate);

        // Trigger 20 consecutive clip transitions mid-fade (as happens during rapid state changes).
        for gen in 1..=20u32 {
            {
                let mut m = world.resource_mut::<Motion>();
                m.fade.generation = gen;
                m.fade.t = 0.0;
            }
            schedule.run(&mut world);
        }

        let rot = world.get::<Transform>(joint).unwrap().rotation;
        let expected = Quat::from_rotation_y(45.0_f32.to_radians())
            * Quat::from_rotation_x(-10.0_f32.to_radians())
            * rest_q;
        let angle_err = rot.angle_between(expected).to_degrees();
        assert!(
            angle_err < 1.0,
            "CoreControl compounded across clip transitions: err={angle_err:.2} deg"
        );
    }
}
