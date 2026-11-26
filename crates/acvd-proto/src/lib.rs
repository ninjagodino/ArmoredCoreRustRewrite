//! GPU-free wire contract for ACVD multiplayer.
//!
//! This crate owns the protocol version, the LSB-first bit codec, snapshot quantizers, and every
//! message struct. It depends only on `glam` and `bytes`; no sim, transport, or Bevy types leak
//! into the wire format.

pub mod bit;
pub mod error;
pub mod ids;
pub mod messages;
pub mod quant;

pub use bit::{BitReader, BitWriter};
pub use error::ProtoError;
pub use ids::NetId;
pub use messages::{
    ac_flags, combat_flags, combat_msg, command_kind, hot_msg, input_buttons, reliable_msg,
    snapshot_flags, AcRecord, ClientCommandPacket, CombatPacket, DamageEvent, DestructibleBreak,
    FireEvent, HotMessage, InputFrame, InputPacket, JoinAccepted, KillEvent, Loadout,
    LoadoutUpdate, LocalPlayerState, MatchState, MissileCorrection, PlayerJoined, PlayerLeft,
    ProjectileExpire, ProjectileHit, PropState, ReliableMessage, RespawnDrop, SnapshotPacket,
    SnapshotSplit, Wire, WorldState, AC_RECORD_BYTES, INPUT_FRAME_BYTES, MAX_INPUT_FRAMES,
    MAX_LOADOUT_BYTES, MAX_SNAPSHOT_BYTES, PROTOCOL_VERSION,
};
pub use quant::{
    dequantize_coord, dequantize_dir3, dequantize_input_pitch, dequantize_pitch, dequantize_vel,
    dequantize_yaw, quantize_coord, quantize_dir3, quantize_input_pitch, quantize_pitch,
    quantize_vel, quantize_yaw, wrap_angle, PosQuantizer, COORD_SCALE, VEL_SCALE,
};

/// Default game port for the dedicated/listen server.
pub const DEFAULT_PORT: u16 = 5000;
/// Netcode protocol id derived from the ACVD title id and protocol revision.
pub const NETCODE_PROTOCOL_ID: u64 = 0x4E4D_3438_0000_0001;
