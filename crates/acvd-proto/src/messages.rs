//! Wire messages.
//!
//! Field order and widths are part of the protocol; the golden tests in `tests/golden.rs` pin the
//! sizes the server and client budget against.

use bytes::Bytes;

use crate::bit::{BitReader, BitWriter};
use crate::error::ProtoError;
use crate::ids::NetId;

/// Bumped whenever a field changes meaning or order.
pub const PROTOCOL_VERSION: u16 = 3;
/// One UDP datagram, with room under a 1200-byte path MTU.
pub const MAX_SNAPSHOT_BYTES: usize = 1100;
/// Input frames sent per packet (current + last two unacked).
pub const MAX_INPUT_FRAMES: usize = 3;
/// Largest loadout blob carried by join/loadout messages.
pub const MAX_LOADOUT_BYTES: usize = 512;

/// Encoded length of one [`InputFrame`] in bytes.
pub const INPUT_FRAME_BYTES: usize = 9;
/// Encoded length of one [`AcRecord`] in bytes.
pub const AC_RECORD_BYTES: usize = 28;

/// `InputFrame::buttons` bits (movement + combat inputs).
pub mod input_buttons {
    pub const JUMP: u16 = 1 << 0;
    pub const BOOST_TOGGLE: u16 = 1 << 1;
    pub const GLIDE: u16 = 1 << 2;
    pub const QUICK_BOOST: u16 = 1 << 3;
    pub const QB_HELD: u16 = 1 << 4;
    pub const KICK: u16 = 1 << 5;
    pub const FIRE_R: u16 = 1 << 6;
    pub const FIRE_L: u16 = 1 << 7;
    pub const FIRE_SHOULDER: u16 = 1 << 8;
    /// Rack swap requests sampled from the bay-shift modifier + that hand's trigger.
    pub const BAY_R: u16 = 1 << 9;
    pub const BAY_L: u16 = 1 << 10;
    /// Overed Weapon trigger (arming/fire is resolved by the server's OW state machine).
    pub const FIRE_OW: u16 = 1 << 11;
}

/// `AcRecord::combat_flags` bits (one compact byte replicated for remote weapon presentation).
pub mod combat_flags {
    pub const LEFT_BAY_ACTIVE: u8 = 1 << 0;
    pub const RIGHT_BAY_ACTIVE: u8 = 1 << 1;
    pub const LEFT_SWAPPING: u8 = 1 << 2;
    pub const RIGHT_SWAPPING: u8 = 1 << 3;
    /// Network glide quick-boost replay flag carried next to the bay bits.
    pub const GLIDE_QB: u8 = 1 << 4;
    /// Bits 5..=7 carry the authoritative weapon-arm phase (`WeaponArmPhase::code`).
    pub const WEAPON_ARM_PHASE_SHIFT: u8 = 5;
    pub const WEAPON_ARM_PHASE_MASK: u8 = 0xE0;
}

/// Tag byte at the start of every `HOT` payload. `HOT` carries snapshots, `FireEvent`s, and the
/// `LocalPlayerState`-embedded missile corrections; there are no per-bullet transform messages.
pub mod hot_msg {
    pub const SNAPSHOT: u8 = 0;
    pub const FIRE: u8 = 1;
}

/// `AcRecord::flags` bits.
pub mod ac_flags {
    pub const BOOST: u16 = 1 << 0;
    pub const GLIDE: u16 = 1 << 1;
    pub const AIRBORNE: u16 = 1 << 2;
    pub const SCAN: u16 = 1 << 3;
    pub const QB: u16 = 1 << 4;
    pub const WALL_JUMP: u16 = 1 << 5;
    pub const KICK: u16 = 1 << 6;
    pub const BLADE: u16 = 1 << 7;
    pub const RELOAD_L: u16 = 1 << 8;
    pub const RELOAD_R: u16 = 1 << 9;
    pub const READY_L: u16 = 1 << 10;
    pub const READY_R: u16 = 1 << 11;
    pub const OVERED: u16 = 1 << 12;
    pub const DESTROYED: u16 = 1 << 13;
    pub const TEAM_B: u16 = 1 << 14;
    pub const INVULNERABLE: u16 = 1 << 15;
}

/// `ClientCommandPacket::kind` values.
pub mod command_kind {
    pub const JOIN: u8 = 0;
    pub const LOADOUT: u8 = 1;
    pub const RESPAWN: u8 = 2;
    pub const TEAM: u8 = 3;
}

/// `SnapshotPacket::flags` bits.
pub mod snapshot_flags {
    pub const HAS_LOCAL: u8 = 1 << 0;
}

/// `CombatPacket` tags on the reliable-unordered `COMBAT` channel.
pub mod combat_msg {
    pub const DAMAGE: u8 = 1;
    pub const HIT: u8 = 2;
    pub const EXPIRE: u8 = 3;
}

/// `ReliableMessage` tags (lifecycle/world transitions share one ordered channel).
pub mod reliable_msg {
    pub const PLAYER_JOINED: u8 = 1;
    pub const PLAYER_LEFT: u8 = 2;
    pub const LOADOUT_UPDATE: u8 = 3;
    pub const MATCH_STATE: u8 = 4;
    pub const KILL_EVENT: u8 = 5;
    pub const RESPAWN_DROP: u8 = 6;
    pub const DESTRUCTIBLE_BREAK: u8 = 7;
    pub const WORLD_STATE: u8 = 8;
}

/// A fixed-layout wire message.
pub trait Wire: Sized {
    fn encode(&self, w: &mut BitWriter);
    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError>;

    fn to_bytes(&self) -> Bytes {
        let mut w = BitWriter::new();
        self.encode(&mut w);
        w.finish()
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = BitReader::new(bytes);
        let msg = Self::decode(&mut r)?;
        r.finish()?;
        Ok(msg)
    }
}

/// One tick of player input without its sequence number (9 B).
///
/// `InputPacket` carries one sequence for its first frame; the rest are consecutive, so a frame
/// does not repeat `seq` on the wire.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputFrame {
    pub buttons: u16,
    pub stick: [i8; 2],
    pub aim_yaw: u16,
    pub aim_pitch: i16,
    pub lock_target: u8,
}

impl InputFrame {
    pub const WIRE_BYTES: usize = INPUT_FRAME_BYTES;

    /// Copy safe for the server's starvation repeat: one-shot button edges are cleared, holds
    /// (Quick Boost charge) stay.
    pub fn repeat_safe(&self) -> Self {
        let edges = input_buttons::JUMP
            | input_buttons::BOOST_TOGGLE
            | input_buttons::GLIDE
            | input_buttons::QUICK_BOOST
            | input_buttons::KICK
            | input_buttons::BAY_R
            | input_buttons::BAY_L;
        Self {
            buttons: self.buttons & !edges,
            ..*self
        }
    }
}

impl Wire for InputFrame {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u16(self.buttons);
        w.write_i8(self.stick[0]);
        w.write_i8(self.stick[1]);
        w.write_u16(self.aim_yaw);
        w.write_i16(self.aim_pitch);
        w.write_u8(self.lock_target);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            buttons: r.read_u16()?,
            stick: [r.read_i8()?, r.read_i8()?],
            aim_yaw: r.read_u16()?,
            aim_pitch: r.read_i16()?,
            lock_target: r.read_u8()?,
        })
    }
}

/// One to three consecutive input frames starting at `start_seq`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputPacket {
    pub start_seq: u16,
    pub frames: Vec<InputFrame>,
}

impl Wire for InputPacket {
    fn encode(&self, w: &mut BitWriter) {
        debug_assert!((1..=MAX_INPUT_FRAMES).contains(&self.frames.len()));
        w.write_u16(self.start_seq);
        w.write_u8(self.frames.len() as u8);
        for frame in &self.frames {
            frame.encode(w);
        }
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        let start_seq = r.read_u16()?;
        let count = r.read_u8()?;
        if !(1..=MAX_INPUT_FRAMES as u8).contains(&count) {
            return Err(ProtoError::Invalid("input frame count"));
        }
        let mut frames = Vec::with_capacity(count as usize);
        for _ in 0..count {
            frames.push(InputFrame::decode(r)?);
        }
        Ok(Self { start_seq, frames })
    }
}

/// Part IDs of one assembled AC plus its colour/emblem ids (38 B).
///
/// The order matches the `AC_ASSEMBLY_DESIGN_ST` movement frame slots the client assembles and
/// the server turns into `AcCtrlParam`: head, core, arms, legs, fcs, generator, booster,
/// back-boost, side-boost, over-boost, right arm, left arm, right back, left back, shoulder,
/// right hanger, left hanger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Loadout {
    pub parts: [i16; Loadout::PARTS],
    pub coloring_id: u16,
    pub emblem_id: u16,
}

impl Loadout {
    pub const PARTS: usize = 17;
    pub const WIRE_BYTES: usize = Self::PARTS * 2 + 4;

    /// Index of each `AC_ASSEMBLY_DESIGN_ST` field in [`Loadout::parts`].
    pub const HEAD: usize = 0;
    pub const CORE: usize = 1;
    pub const ARMS: usize = 2;
    pub const LEGS: usize = 3;
    pub const FCS: usize = 4;
    pub const GENERATOR: usize = 5;
    pub const BOOSTER: usize = 6;
    pub const BACK_BOOST: usize = 7;
    pub const SIDE_BOOST: usize = 8;
    pub const OVER_BOOST: usize = 9;
    pub const ARM_R: usize = 10;
    pub const ARM_L: usize = 11;
    pub const BACK_R: usize = 12;
    pub const BACK_L: usize = 13;
    pub const SHOULDER: usize = 14;
    pub const HANGER_R: usize = 15;
    pub const HANGER_L: usize = 16;

    /// Decodes a `Loadout` from a `ClientCommandPacket`/`LoadoutUpdate` blob.
    pub fn from_blob(bytes: &[u8]) -> Result<Self, ProtoError> {
        Self::from_bytes(bytes)
    }

    pub fn to_blob(self) -> Vec<u8> {
        self.to_bytes().to_vec()
    }
}

impl Wire for Loadout {
    fn encode(&self, w: &mut BitWriter) {
        for part in self.parts {
            w.write_i16(part);
        }
        w.write_u16(self.coloring_id);
        w.write_u16(self.emblem_id);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        let mut parts = [0i16; Self::PARTS];
        for part in &mut parts {
            *part = r.read_i16()?;
        }
        Ok(Self {
            parts,
            coloring_id: r.read_u16()?,
            emblem_id: r.read_u16()?,
        })
    }
}

/// One remote AC in a snapshot (28 B, still under the 32 B cap).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AcRecord {
    pub net_id: NetId,
    pub pos: [u16; 3],
    pub vel: [i16; 3],
    pub yaw: u16,
    pub pitch: i8,
    pub flags: u16,
    pub pose_state: u8,
    /// Bay/rack active + swap state, `glide_qb`, and the weapon-arm phase bits
    /// ([`combat_flags`]).
    pub combat_flags: u8,
    /// Remaining 30 Hz wire ticks of an active weapon-arm deploy/stow/fire transition (expanded to 60 Hz clip frames by the sim).
    pub weapon_arm_ticks: u8,
    pub ap: u16,
    pub en: u8,
    pub reload: [u8; 2],
    pub ammo: [u8; 2],
}

impl AcRecord {
    pub const WIRE_BYTES: usize = AC_RECORD_BYTES;
    pub const MAX_WIRE_BYTES: usize = 32;
    /// Ceiling from the plan for future optional fields.
    pub const HARD_CEILING: usize = 48;
}

impl Wire for AcRecord {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u8(self.net_id.get());
        for q in self.pos {
            w.write_u16(q);
        }
        for v in self.vel {
            w.write_i16(v);
        }
        w.write_u16(self.yaw);
        w.write_i8(self.pitch);
        w.write_u16(self.flags);
        w.write_u8(self.pose_state);
        w.write_u8(self.combat_flags);
        w.write_u8(self.weapon_arm_ticks);
        w.write_u16(self.ap);
        w.write_u8(self.en);
        w.write_u8(self.reload[0]);
        w.write_u8(self.reload[1]);
        w.write_u8(self.ammo[0]);
        w.write_u8(self.ammo[1]);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            net_id: NetId::new(r.read_u8()?)?,
            pos: [r.read_u16()?, r.read_u16()?, r.read_u16()?],
            vel: [r.read_i16()?, r.read_i16()?, r.read_i16()?],
            yaw: r.read_u16()?,
            pitch: r.read_i8()?,
            flags: r.read_u16()?,
            pose_state: r.read_u8()?,
            combat_flags: r.read_u8()?,
            weapon_arm_ticks: r.read_u8()?,
            ap: r.read_u16()?,
            en: r.read_u8()?,
            reload: [r.read_u8()?, r.read_u8()?],
            ammo: [r.read_u8()?, r.read_u8()?],
        })
    }
}

/// Full authoritative state of the recording client (229 B), always in snapshot packet 1.
///
/// This is the state reconciliation replays from. It carries every movement phase and cooldown
/// timer `AcMotion` owns, plus the phase parameters that a bare timer cannot rebuild (Quick Boost
/// phase lengths/direction, Wall Jump direction/normal, Kick and Blade directions).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LocalPlayerState {
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    /// Body facing (radians).
    pub yaw: f32,
    /// Absolute aim pitch (radians).
    pub pitch: f32,
    /// Absolute aim yaw (radians).
    pub aim_yaw: f32,
    pub flags: u16,
    pub en: f32,
    pub en_max: f32,
    pub ap: f32,
    pub last_acked_seq: u16,
    /// Server tick this state was recorded at; prediction replays forward from here.
    pub server_tick: u32,
    /// Same compact bay/swap/glide-qb + weapon-arm phase bits as [`AcRecord::combat_flags`].
    pub combat_flags: u8,
    /// Remaining 30 Hz wire ticks of the active weapon-arm transition (expanded to 60 Hz clip frames by the sim).
    pub weapon_arm_ticks: u8,
    /// Alternating weapon-arm barrel/muzzle index.
    pub weapon_arm_barrel: u8,
    /// Continue the weapon-arm clip from the exact local frame (reconciliation only).
    pub weapon_arm_frame: f32,
    /// Remaining ticks of each hand's bay swap (`0` when idle).
    pub swap_ticks: [u16; 2],
    /// Magazine counters for the hand, bay, and shoulder weapons.
    pub hand_ammo: [u16; 2],
    pub bay_ammo: [u16; 2],
    pub shoulder_ammo: u16,
    /// Remaining cooldown ticks (also the reload lockout) for each weapon slot.
    pub hand_cooldown: [f32; 2],
    pub bay_cooldown: [f32; 2],
    pub shoulder_cooldown: f32,
    /// Charge accumulation in ticks for charge weapons.
    pub hand_charge: [f32; 2],
    pub bay_charge: [f32; 2],
    pub shoulder_charge: f32,
    /// Sequential burst state for the hand weapons.
    pub burst_remaining: [u8; 2],
    pub burst_timer: [f32; 2],
    /// Ticks remaining in the active movement phases (`0` when inactive).
    pub qb_timer: f32,
    pub wj_timer: f32,
    pub kick_timer: f32,
    pub blade_timer: f32,
    pub stagger_timer: f32,
    pub en_regen_delay: f32,
    pub glide_time: f32,
    pub glide_lift_left: f32,
    pub glide_activation_y: f32,
    pub takeoff: f32,
    pub qb_cooldown: f32,
    pub wj_lockout: f32,
    pub ow_dash_ticks: f32,
    /// `HighBoostState` phase parameters (`qb_phase`: 0 none, 1 charge, 2 burst 1, 3 burst 2).
    pub qb_weight: f32,
    pub qb_p1_ticks: f32,
    pub qb_p2_ticks: f32,
    pub qb_charge_held: f32,
    pub qb_dir: [i8; 3],
    /// Wall Jump phase direction and contact normal (unit i8 components).
    pub wj_dir: [i8; 3],
    pub wj_normal: [i8; 3],
    /// Kick direction (horizontal unit i8 components).
    pub kick_dir: [i8; 3],
    /// Blade lunge target (0.01 m units) and phase parameters.
    pub blade_target: [i16; 3],
    pub blade_max_dash_ticks: f32,
    pub blade_entry_speed: f32,
    pub qb_phase: u8,
    pub wj_phase: u8,
    pub blade_phase: u8,
    pub kick_hit: u8,
    pub glide_phase: u8,
    /// Bit 0: start ascent active, bit 1: start ascent captured.
    pub glide_flags: u8,
    /// Bit 0: blade started airborne, bit 1: downward thrust clip.
    pub blade_flags: u8,
    pub pose_state: u8,
}

impl LocalPlayerState {
    pub const WIRE_BYTES: usize = 229;
}

impl Wire for LocalPlayerState {
    fn encode(&self, w: &mut BitWriter) {
        for v in self.pos {
            w.write_f32(v);
        }
        for v in self.vel {
            w.write_f32(v);
        }
        w.write_f32(self.yaw);
        w.write_f32(self.pitch);
        w.write_f32(self.aim_yaw);
        w.write_u16(self.flags);
        w.write_f32(self.en);
        w.write_f32(self.en_max);
        w.write_f32(self.ap);
        w.write_u16(self.last_acked_seq);
        w.write_u32(self.server_tick);
        w.write_u8(self.combat_flags);
        w.write_u8(self.weapon_arm_ticks);
        w.write_u8(self.weapon_arm_barrel);
        w.write_f32(self.weapon_arm_frame);
        for v in self.swap_ticks {
            w.write_u16(v);
        }
        for v in self.hand_ammo {
            w.write_u16(v);
        }
        for v in self.bay_ammo {
            w.write_u16(v);
        }
        w.write_u16(self.shoulder_ammo);
        for v in [
            self.hand_cooldown[0],
            self.hand_cooldown[1],
            self.bay_cooldown[0],
            self.bay_cooldown[1],
            self.shoulder_cooldown,
            self.hand_charge[0],
            self.hand_charge[1],
            self.bay_charge[0],
            self.bay_charge[1],
            self.shoulder_charge,
        ] {
            w.write_f32(v);
        }
        for v in self.burst_remaining {
            w.write_u8(v);
        }
        for v in self.burst_timer {
            w.write_f32(v);
        }
        for v in [
            self.qb_timer,
            self.wj_timer,
            self.kick_timer,
            self.blade_timer,
            self.stagger_timer,
            self.en_regen_delay,
            self.glide_time,
            self.glide_lift_left,
            self.glide_activation_y,
            self.takeoff,
            self.qb_cooldown,
            self.wj_lockout,
            self.ow_dash_ticks,
            self.qb_weight,
            self.qb_p1_ticks,
            self.qb_p2_ticks,
            self.qb_charge_held,
        ] {
            w.write_f32(v);
        }
        for q in self.qb_dir {
            w.write_i8(q);
        }
        for q in self.wj_dir {
            w.write_i8(q);
        }
        for q in self.wj_normal {
            w.write_i8(q);
        }
        for q in self.kick_dir {
            w.write_i8(q);
        }
        for q in self.blade_target {
            w.write_i16(q);
        }
        w.write_f32(self.blade_max_dash_ticks);
        w.write_f32(self.blade_entry_speed);
        w.write_u8(self.qb_phase);
        w.write_u8(self.wj_phase);
        w.write_u8(self.blade_phase);
        w.write_u8(self.kick_hit);
        w.write_u8(self.glide_phase);
        w.write_u8(self.glide_flags);
        w.write_u8(self.blade_flags);
        w.write_u8(self.pose_state);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            pos: [r.read_f32()?, r.read_f32()?, r.read_f32()?],
            vel: [r.read_f32()?, r.read_f32()?, r.read_f32()?],
            yaw: r.read_f32()?,
            pitch: r.read_f32()?,
            aim_yaw: r.read_f32()?,
            flags: r.read_u16()?,
            en: r.read_f32()?,
            en_max: r.read_f32()?,
            ap: r.read_f32()?,
            last_acked_seq: r.read_u16()?,
            server_tick: r.read_u32()?,
            combat_flags: r.read_u8()?,
            weapon_arm_ticks: r.read_u8()?,
            weapon_arm_barrel: r.read_u8()?,
            weapon_arm_frame: r.read_f32()?,
            swap_ticks: [r.read_u16()?, r.read_u16()?],
            hand_ammo: [r.read_u16()?, r.read_u16()?],
            bay_ammo: [r.read_u16()?, r.read_u16()?],
            shoulder_ammo: r.read_u16()?,
            hand_cooldown: [r.read_f32()?, r.read_f32()?],
            bay_cooldown: [r.read_f32()?, r.read_f32()?],
            shoulder_cooldown: r.read_f32()?,
            hand_charge: [r.read_f32()?, r.read_f32()?],
            bay_charge: [r.read_f32()?, r.read_f32()?],
            shoulder_charge: r.read_f32()?,
            burst_remaining: [r.read_u8()?, r.read_u8()?],
            burst_timer: [r.read_f32()?, r.read_f32()?],
            qb_timer: r.read_f32()?,
            wj_timer: r.read_f32()?,
            kick_timer: r.read_f32()?,
            blade_timer: r.read_f32()?,
            stagger_timer: r.read_f32()?,
            en_regen_delay: r.read_f32()?,
            glide_time: r.read_f32()?,
            glide_lift_left: r.read_f32()?,
            glide_activation_y: r.read_f32()?,
            takeoff: r.read_f32()?,
            qb_cooldown: r.read_f32()?,
            wj_lockout: r.read_f32()?,
            ow_dash_ticks: r.read_f32()?,
            qb_weight: r.read_f32()?,
            qb_p1_ticks: r.read_f32()?,
            qb_p2_ticks: r.read_f32()?,
            qb_charge_held: r.read_f32()?,
            qb_dir: [r.read_i8()?, r.read_i8()?, r.read_i8()?],
            wj_dir: [r.read_i8()?, r.read_i8()?, r.read_i8()?],
            wj_normal: [r.read_i8()?, r.read_i8()?, r.read_i8()?],
            kick_dir: [r.read_i8()?, r.read_i8()?, r.read_i8()?],
            blade_target: [r.read_i16()?, r.read_i16()?, r.read_i16()?],
            blade_max_dash_ticks: r.read_f32()?,
            blade_entry_speed: r.read_f32()?,
            qb_phase: r.read_u8()?,
            wj_phase: r.read_u8()?,
            blade_phase: r.read_u8()?,
            kick_hit: r.read_u8()?,
            glide_phase: r.read_u8()?,
            glide_flags: r.read_u8()?,
            blade_flags: r.read_u8()?,
            pose_state: r.read_u8()?,
        })
    }
}

/// Projectile or missile position/velocity correction (17 B).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MissileCorrection {
    pub projectile_id: u32,
    pub pos: [i16; 3],
    pub vel: [i16; 3],
    pub target: NetId,
}

impl MissileCorrection {
    pub const WIRE_BYTES: usize = 17;
}

impl Wire for MissileCorrection {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u32(self.projectile_id);
        for q in self.pos {
            w.write_i16(q);
        }
        for q in self.vel {
            w.write_i16(q);
        }
        w.write_u8(self.target.get());
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            projectile_id: r.read_u32()?,
            pos: [r.read_i16()?, r.read_i16()?, r.read_i16()?],
            vel: [r.read_i16()?, r.read_i16()?, r.read_i16()?],
            target: NetId::from_wire(r.read_u8()?)?,
        })
    }
}

/// A shot; clients spawn the visual locally and the server simulates it (20 B).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FireEvent {
    pub shooter: NetId,
    pub projectile_id: u32,
    pub weapon_slot: u8,
    pub muzzle_pos: [i16; 3],
    pub aim_dir: [i8; 3],
    pub fire_tick: u32,
    pub seed: u8,
    /// Weapon-arm barrel/muzzle index (0/1 alternating L/R; 0 for hand/shoulder guns).
    pub barrel: u8,
}

impl FireEvent {
    pub const WIRE_BYTES: usize = 21;
}

impl Wire for FireEvent {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u8(self.shooter.get());
        w.write_u32(self.projectile_id);
        w.write_u8(self.weapon_slot);
        for q in self.muzzle_pos {
            w.write_i16(q);
        }
        for q in self.aim_dir {
            w.write_i8(q);
        }
        w.write_u32(self.fire_tick);
        w.write_u8(self.seed);
        w.write_u8(self.barrel);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            shooter: NetId::from_wire(r.read_u8()?)?,
            projectile_id: r.read_u32()?,
            weapon_slot: r.read_u8()?,
            muzzle_pos: [r.read_i16()?, r.read_i16()?, r.read_i16()?],
            aim_dir: [r.read_i8()?, r.read_i8()?, r.read_i8()?],
            fire_tick: r.read_u32()?,
            seed: r.read_u8()?,
            barrel: r.read_u8()?,
        })
    }
}

/// Periodic snapshot: header, optional local state, remote AC records, missile corrections.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapshotPacket {
    pub server_tick: u32,
    pub snapshot_seq: u16,
    pub acked_input_seq: u16,
    pub flags: u8,
    pub local: Option<LocalPlayerState>,
    pub records: Vec<AcRecord>,
    pub corrections: Vec<MissileCorrection>,
}

impl SnapshotPacket {
    /// Exact encoded size in bytes (including the `HOT` tag byte).
    pub fn wire_len(&self) -> usize {
        12 + usize::from(self.local.is_some()) * LocalPlayerState::WIRE_BYTES
            + self.records.len() * AcRecord::WIRE_BYTES
            + self.corrections.len() * MissileCorrection::WIRE_BYTES
    }

    pub fn try_encode(&self) -> Result<Bytes, ProtoError> {
        let len = self.wire_len();
        if len > MAX_SNAPSHOT_BYTES {
            return Err(ProtoError::TooLarge {
                name: "SnapshotPacket",
                size: len,
                max: MAX_SNAPSHOT_BYTES,
            });
        }
        Ok(self.to_bytes())
    }

    /// Splits the snapshot into at most two independently decodable packets, each within one
    /// datagram. `LocalPlayerState` stays in the first packet.
    pub fn split_for_udp(&self) -> SnapshotSplit {
        let mut packets = Vec::new();
        let mut rec = 0usize;
        let mut corr = 0usize;
        let mut local = self.local;
        let flags = self.flags & !snapshot_flags::HAS_LOCAL;

        while packets.len() < 2 {
            let mut pkt = SnapshotPacket {
                server_tick: self.server_tick,
                snapshot_seq: self.snapshot_seq,
                acked_input_seq: self.acked_input_seq,
                flags,
                local: local.take(),
                records: Vec::new(),
                corrections: Vec::new(),
            };
            while rec < self.records.len() {
                pkt.records.push(self.records[rec]);
                if pkt.wire_len() > MAX_SNAPSHOT_BYTES {
                    pkt.records.pop();
                    break;
                }
                rec += 1;
            }
            while corr < self.corrections.len() {
                pkt.corrections.push(self.corrections[corr]);
                if pkt.wire_len() > MAX_SNAPSHOT_BYTES {
                    pkt.corrections.pop();
                    break;
                }
                corr += 1;
            }
            if pkt.records.is_empty() && pkt.corrections.is_empty() && pkt.local.is_none() {
                break;
            }
            packets.push(pkt);
            if rec >= self.records.len() && corr >= self.corrections.len() {
                break;
            }
        }

        SnapshotSplit {
            packets,
            dropped_records: self.records.len() - rec,
            dropped_corrections: self.corrections.len() - corr,
        }
    }
}

/// Result of [`SnapshotPacket::split_for_udp`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapshotSplit {
    pub packets: Vec<SnapshotPacket>,
    /// Records beyond the two-packet cap (tier dropping belongs to interest management).
    pub dropped_records: usize,
    pub dropped_corrections: usize,
}

impl Wire for SnapshotPacket {
    fn encode(&self, w: &mut BitWriter) {
        let has_local = self.local.is_some();
        w.write_u8(hot_msg::SNAPSHOT);
        w.write_u32(self.server_tick);
        w.write_u16(self.snapshot_seq);
        w.write_u16(self.acked_input_seq);
        w.write_u8(self.records.len() as u8);
        w.write_u8(self.corrections.len() as u8);
        let flags = self.flags & !snapshot_flags::HAS_LOCAL;
        w.write_u8(if has_local {
            flags | snapshot_flags::HAS_LOCAL
        } else {
            flags
        });
        if let Some(local) = &self.local {
            local.encode(w);
        }
        for record in &self.records {
            record.encode(w);
        }
        for correction in &self.corrections {
            correction.encode(w);
        }
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        let tag = r.read_u8()?;
        if tag != hot_msg::SNAPSHOT {
            return Err(ProtoError::Invalid("snapshot hot tag"));
        }
        Self::decode_body(r)
    }
}

impl SnapshotPacket {
    fn decode_body(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        let server_tick = r.read_u32()?;
        let snapshot_seq = r.read_u16()?;
        let acked_input_seq = r.read_u16()?;
        let ac_count = r.read_u8()?;
        let correction_count = r.read_u8()?;
        let raw_flags = r.read_u8()?;
        if raw_flags & !snapshot_flags::HAS_LOCAL != 0 {
            return Err(ProtoError::Invalid("snapshot flags"));
        }
        let flags = raw_flags & !snapshot_flags::HAS_LOCAL;
        let local = if raw_flags & snapshot_flags::HAS_LOCAL != 0 {
            Some(LocalPlayerState::decode(r)?)
        } else {
            None
        };
        let mut records = Vec::with_capacity(ac_count as usize);
        for _ in 0..ac_count {
            records.push(AcRecord::decode(r)?);
        }
        let mut corrections = Vec::with_capacity(correction_count as usize);
        for _ in 0..correction_count {
            corrections.push(MissileCorrection::decode(r)?);
        }
        Ok(Self {
            server_tick,
            snapshot_seq,
            acked_input_seq,
            flags,
            local,
            records,
            corrections,
        })
    }
}

/// One decoded `HOT` payload.
#[derive(Clone, Debug, PartialEq)]
pub enum HotMessage {
    Snapshot(SnapshotPacket),
    Fire(FireEvent),
}

impl HotMessage {
    pub fn to_bytes(&self) -> Bytes {
        match self {
            Self::Snapshot(packet) => packet.to_bytes(),
            Self::Fire(event) => {
                let mut w = BitWriter::with_capacity(FireEvent::WIRE_BYTES + 1);
                w.write_u8(hot_msg::FIRE);
                event.encode(&mut w);
                w.finish()
            }
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = BitReader::new(bytes);
        let tag = r.read_u8()?;
        let message = match tag {
            hot_msg::SNAPSHOT => Self::Snapshot(SnapshotPacket::decode_body(&mut r)?),
            hot_msg::FIRE => Self::Fire(FireEvent::decode(&mut r)?),
            _ => return Err(ProtoError::Invalid("hot message tag")),
        };
        r.finish()?;
        Ok(message)
    }
}

/// Coalesced combat feedback for one server tick on the `COMBAT` channel.
///
/// `damages` carries at most one entry per damaged target (amounts summed, effective/lethal ORed),
/// so a gatling burst cannot flood the reliable-unordered channel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CombatPacket {
    pub damages: Vec<DamageEvent>,
    pub hits: Vec<ProjectileHit>,
    pub expires: Vec<ProjectileExpire>,
}

impl CombatPacket {
    pub fn is_empty(&self) -> bool {
        self.damages.is_empty() && self.hits.is_empty() && self.expires.is_empty()
    }

    pub fn to_bytes(&self) -> Bytes {
        let mut w = BitWriter::with_capacity(
            4 + (self.damages.len() + self.hits.len() + self.expires.len()) * 20,
        );
        w.write_u8(self.damages.len().min(u8::MAX as usize) as u8);
        for damage in self.damages.iter().take(u8::MAX as usize) {
            w.write_u8(combat_msg::DAMAGE);
            damage.encode(&mut w);
        }
        w.write_u8(self.hits.len().min(u8::MAX as usize) as u8);
        for hit in self.hits.iter().take(u8::MAX as usize) {
            w.write_u8(combat_msg::HIT);
            hit.encode(&mut w);
        }
        w.write_u8(self.expires.len().min(u8::MAX as usize) as u8);
        for expire in self.expires.iter().take(u8::MAX as usize) {
            w.write_u8(combat_msg::EXPIRE);
            expire.encode(&mut w);
        }
        w.finish()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = BitReader::new(bytes);
        let damage_count = r.read_u8()? as usize;
        let mut damages = Vec::with_capacity(damage_count);
        for _ in 0..damage_count {
            let tag = r.read_u8()?;
            if tag != combat_msg::DAMAGE {
                return Err(ProtoError::Invalid("combat damage tag"));
            }
            damages.push(DamageEvent::decode(&mut r)?);
        }
        let hit_count = r.read_u8()? as usize;
        let mut hits = Vec::with_capacity(hit_count);
        for _ in 0..hit_count {
            let tag = r.read_u8()?;
            if tag != combat_msg::HIT {
                return Err(ProtoError::Invalid("combat hit tag"));
            }
            hits.push(ProjectileHit::decode(&mut r)?);
        }
        let expire_count = r.read_u8()? as usize;
        let mut expires = Vec::with_capacity(expire_count);
        for _ in 0..expire_count {
            let tag = r.read_u8()?;
            if tag != combat_msg::EXPIRE {
                return Err(ProtoError::Invalid("combat expire tag"));
            }
            expires.push(ProjectileExpire::decode(&mut r)?);
        }
        r.finish()?;
        Ok(Self {
            damages,
            hits,
            expires,
        })
    }
}

/// Client -> server reliable command (join/loadout/respawn/team).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientCommandPacket {
    pub seq: u16,
    pub kind: u8,
    pub protocol: u16,
    pub loadout: Vec<u8>,
}

impl Wire for ClientCommandPacket {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u16(self.seq);
        w.write_u8(self.kind);
        w.write_u16(self.protocol);
        w.write_u16(self.loadout.len() as u16);
        for byte in &self.loadout {
            w.write_u8(*byte);
        }
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        let seq = r.read_u16()?;
        let kind = r.read_u8()?;
        let protocol = r.read_u16()?;
        let len = r.read_u16()? as usize;
        if len > MAX_LOADOUT_BYTES {
            return Err(ProtoError::Invalid("loadout length"));
        }
        if kind > command_kind::TEAM {
            return Err(ProtoError::Invalid("command kind"));
        }
        let mut loadout = Vec::with_capacity(len);
        for _ in 0..len {
            loadout.push(r.read_u8()?);
        }
        Ok(Self {
            seq,
            kind,
            protocol,
            loadout,
        })
    }
}

/// Server -> client accepted join, with the map AABB used by snapshot quantizers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JoinAccepted {
    pub protocol: u16,
    pub net_id: NetId,
    pub server_tick: u32,
    pub map_id: u32,
    pub pos_min: [f32; 3],
    pub pos_max: [f32; 3],
    pub loadout: Vec<u8>,
}

impl Wire for JoinAccepted {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u16(self.protocol);
        w.write_u8(self.net_id.get());
        w.write_u32(self.server_tick);
        w.write_u32(self.map_id);
        for v in self.pos_min {
            w.write_f32(v);
        }
        for v in self.pos_max {
            w.write_f32(v);
        }
        w.write_u16(self.loadout.len() as u16);
        for byte in &self.loadout {
            w.write_u8(*byte);
        }
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        let protocol = r.read_u16()?;
        let net_id = NetId::new(r.read_u8()?)?;
        let server_tick = r.read_u32()?;
        let map_id = r.read_u32()?;
        let pos_min = [r.read_f32()?, r.read_f32()?, r.read_f32()?];
        let pos_max = [r.read_f32()?, r.read_f32()?, r.read_f32()?];
        let len = r.read_u16()? as usize;
        if len > MAX_LOADOUT_BYTES {
            return Err(ProtoError::Invalid("loadout length"));
        }
        let mut loadout = Vec::with_capacity(len);
        for _ in 0..len {
            loadout.push(r.read_u8()?);
        }
        Ok(Self {
            protocol,
            net_id,
            server_tick,
            map_id,
            pos_min,
            pos_max,
            loadout,
        })
    }
}

/// Another player joined the match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerJoined {
    pub net_id: NetId,
    pub team: u8,
}

impl Wire for PlayerJoined {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u8(self.net_id.get());
        w.write_u8(self.team);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            net_id: NetId::new(r.read_u8()?)?,
            team: r.read_u8()?,
        })
    }
}

/// A player left the match.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerLeft {
    pub net_id: NetId,
    pub reason: u8,
}

impl Wire for PlayerLeft {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u8(self.net_id.get());
        w.write_u8(self.reason);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            net_id: NetId::new(r.read_u8()?)?,
            reason: r.read_u8()?,
        })
    }
}

/// A player's loadout changed (join/respawn only).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadoutUpdate {
    pub net_id: NetId,
    pub loadout: Vec<u8>,
}

impl Wire for LoadoutUpdate {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u8(self.net_id.get());
        w.write_u16(self.loadout.len() as u16);
        for byte in &self.loadout {
            w.write_u8(*byte);
        }
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        let net_id = NetId::new(r.read_u8()?)?;
        let len = r.read_u16()? as usize;
        if len > MAX_LOADOUT_BYTES {
            return Err(ProtoError::Invalid("loadout length"));
        }
        let mut loadout = Vec::with_capacity(len);
        for _ in 0..len {
            loadout.push(r.read_u8()?);
        }
        Ok(Self { net_id, loadout })
    }
}

/// Round/match state transitions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatchState {
    pub state: u8,
    pub score_a: u16,
    pub score_b: u16,
    pub time_left_ticks: u32,
}

impl Wire for MatchState {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u8(self.state);
        w.write_u16(self.score_a);
        w.write_u16(self.score_b);
        w.write_u32(self.time_left_ticks);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            state: r.read_u8()?,
            score_a: r.read_u16()?,
            score_b: r.read_u16()?,
            time_left_ticks: r.read_u32()?,
        })
    }
}

/// A kill; killer `0` means environment/self.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KillEvent {
    pub killer: NetId,
    pub victim: NetId,
    pub weapon: u8,
    pub tick: u32,
}

impl Wire for KillEvent {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u8(self.killer.get());
        w.write_u8(self.victim.get());
        w.write_u8(self.weapon);
        w.write_u32(self.tick);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            killer: NetId::from_wire(r.read_u8()?)?,
            victim: NetId::new(r.read_u8()?)?,
            weapon: r.read_u8()?,
            tick: r.read_u32()?,
        })
    }
}

/// Respawn point chosen by the server. Clients pause prediction until `start_tick` and present
/// the drop-in at `drop_point` facing `approach_yaw`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RespawnDrop {
    pub net_id: NetId,
    pub drop_point: [f32; 3],
    pub approach_yaw: f32,
    pub start_tick: u32,
}

impl Wire for RespawnDrop {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u8(self.net_id.get());
        for v in self.drop_point {
            w.write_f32(v);
        }
        w.write_f32(self.approach_yaw);
        w.write_u32(self.start_tick);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            net_id: NetId::new(r.read_u8()?)?,
            drop_point: [r.read_f32()?, r.read_f32()?, r.read_f32()?],
            approach_yaw: r.read_f32()?,
            start_tick: r.read_u32()?,
        })
    }
}

/// Damaged/broken destructible state for late joiners (`hp` is omitted for untouched props).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PropState {
    pub prop_id: u32,
    pub hp: f32,
    pub broken: bool,
}

impl PropState {
    pub const WIRE_BYTES: usize = 9;
}

impl Wire for PropState {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u32(self.prop_id);
        w.write_f32(self.hp);
        w.write_bool(self.broken);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            prop_id: r.read_u32()?,
            hp: r.read_f32()?,
            broken: r.read_bool()?,
        })
    }
}

/// Late-join world state: the full destructible broken bitset plus the damaged HP array.
///
/// `props` carries one entry per destructible whose hp differs from its spawn value (and every
/// broken prop); `broken_bits` is the complete bitset in `prop_id` order, so a late joiner matches
/// the early joiner's collision state even for props that broke without passing through a delta.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorldState {
    pub server_tick: u32,
    pub props: Vec<PropState>,
    pub broken_bits: Vec<u8>,
}

impl WorldState {
    pub fn is_broken(&self, index: usize) -> bool {
        self.broken_bits
            .get(index / 8)
            .is_some_and(|byte| byte & (1 << (index % 8)) != 0)
    }
}

impl Wire for WorldState {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u32(self.server_tick);
        w.write_u16(self.props.len().min(u16::MAX as usize) as u16);
        for prop in self.props.iter().take(u16::MAX as usize) {
            prop.encode(w);
        }
        w.write_u16(self.broken_bits.len().min(u16::MAX as usize) as u16);
        for byte in self.broken_bits.iter().take(u16::MAX as usize) {
            w.write_u8(*byte);
        }
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        let server_tick = r.read_u32()?;
        let count = r.read_u16()? as usize;
        let mut props = Vec::with_capacity(count);
        for _ in 0..count {
            props.push(PropState::decode(r)?);
        }
        let bits = r.read_u16()? as usize;
        let mut broken_bits = Vec::with_capacity(bits);
        for _ in 0..bits {
            broken_bits.push(r.read_u8()?);
        }
        Ok(Self {
            server_tick,
            props,
            broken_bits,
        })
    }
}

/// A destructible prop broke.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DestructibleBreak {
    pub prop_id: u32,
    pub tick: u32,
}

impl Wire for DestructibleBreak {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u32(self.prop_id);
        w.write_u32(self.tick);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            prop_id: r.read_u32()?,
            tick: r.read_u32()?,
        })
    }
}

/// Combat feedback; clients apply it idempotently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DamageEvent {
    pub target: NetId,
    pub attacker: NetId,
    pub amount: u16,
    pub attribute: u8,
    pub effective: u8,
    pub lethal: bool,
    pub hit_pos: [i16; 3],
    pub tick: u32,
}

impl Wire for DamageEvent {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u8(self.target.get());
        w.write_u8(self.attacker.get());
        w.write_u16(self.amount);
        w.write_u8(self.attribute);
        w.write_u8(self.effective);
        w.write_bool(self.lethal);
        for q in self.hit_pos {
            w.write_i16(q);
        }
        w.write_u32(self.tick);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            target: NetId::new(r.read_u8()?)?,
            attacker: NetId::from_wire(r.read_u8()?)?,
            amount: r.read_u16()?,
            attribute: r.read_u8()?,
            effective: r.read_u8()?,
            lethal: r.read_bool()?,
            hit_pos: [r.read_i16()?, r.read_i16()?, r.read_i16()?],
            tick: r.read_u32()?,
        })
    }
}

/// A projectile hit (client visual correction).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectileHit {
    pub projectile_id: u32,
    pub target: NetId,
    pub hit_pos: [i16; 3],
    pub tick: u32,
}

impl Wire for ProjectileHit {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u32(self.projectile_id);
        w.write_u8(self.target.get());
        for q in self.hit_pos {
            w.write_i16(q);
        }
        w.write_u32(self.tick);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            projectile_id: r.read_u32()?,
            target: NetId::new(r.read_u8()?)?,
            hit_pos: [r.read_i16()?, r.read_i16()?, r.read_i16()?],
            tick: r.read_u32()?,
        })
    }
}

/// A projectile expired without a hit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectileExpire {
    pub projectile_id: u32,
    pub tick: u32,
}

impl Wire for ProjectileExpire {
    fn encode(&self, w: &mut BitWriter) {
        w.write_u32(self.projectile_id);
        w.write_u32(self.tick);
    }

    fn decode(r: &mut BitReader<'_>) -> Result<Self, ProtoError> {
        Ok(Self {
            projectile_id: r.read_u32()?,
            tick: r.read_u32()?,
        })
    }
}

/// Tagged messages on the reliable-ordered lifecycle channel.
#[derive(Clone, Debug, PartialEq)]
pub enum ReliableMessage {
    PlayerJoined(PlayerJoined),
    PlayerLeft(PlayerLeft),
    LoadoutUpdate(LoadoutUpdate),
    MatchState(MatchState),
    KillEvent(KillEvent),
    RespawnDrop(RespawnDrop),
    DestructibleBreak(DestructibleBreak),
    WorldState(WorldState),
}

impl ReliableMessage {
    pub fn to_bytes(&self) -> Bytes {
        let mut w = BitWriter::with_capacity(64);
        match self {
            Self::PlayerJoined(m) => {
                w.write_u8(reliable_msg::PLAYER_JOINED);
                m.encode(&mut w);
            }
            Self::PlayerLeft(m) => {
                w.write_u8(reliable_msg::PLAYER_LEFT);
                m.encode(&mut w);
            }
            Self::LoadoutUpdate(m) => {
                w.write_u8(reliable_msg::LOADOUT_UPDATE);
                m.encode(&mut w);
            }
            Self::MatchState(m) => {
                w.write_u8(reliable_msg::MATCH_STATE);
                m.encode(&mut w);
            }
            Self::KillEvent(m) => {
                w.write_u8(reliable_msg::KILL_EVENT);
                m.encode(&mut w);
            }
            Self::RespawnDrop(m) => {
                w.write_u8(reliable_msg::RESPAWN_DROP);
                m.encode(&mut w);
            }
            Self::DestructibleBreak(m) => {
                w.write_u8(reliable_msg::DESTRUCTIBLE_BREAK);
                m.encode(&mut w);
            }
            Self::WorldState(m) => {
                w.write_u8(reliable_msg::WORLD_STATE);
                m.encode(&mut w);
            }
        }
        w.finish()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = BitReader::new(bytes);
        let id = r.read_u8()?;
        let message = match id {
            reliable_msg::PLAYER_JOINED => Self::PlayerJoined(PlayerJoined::decode(&mut r)?),
            reliable_msg::PLAYER_LEFT => Self::PlayerLeft(PlayerLeft::decode(&mut r)?),
            reliable_msg::LOADOUT_UPDATE => Self::LoadoutUpdate(LoadoutUpdate::decode(&mut r)?),
            reliable_msg::MATCH_STATE => Self::MatchState(MatchState::decode(&mut r)?),
            reliable_msg::KILL_EVENT => Self::KillEvent(KillEvent::decode(&mut r)?),
            reliable_msg::RESPAWN_DROP => Self::RespawnDrop(RespawnDrop::decode(&mut r)?),
            reliable_msg::DESTRUCTIBLE_BREAK => {
                Self::DestructibleBreak(DestructibleBreak::decode(&mut r)?)
            }
            reliable_msg::WORLD_STATE => Self::WorldState(WorldState::decode(&mut r)?),
            _ => return Err(ProtoError::Invalid("reliable message tag")),
        };
        r.finish()?;
        Ok(message)
    }
}
