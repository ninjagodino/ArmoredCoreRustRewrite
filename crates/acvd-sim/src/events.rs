//! Events emitted by one simulation tick. The server turns these into wire messages; the
//! offline client turns them into the same presentation as single-player.

use glam::Vec3;

use crate::ids::{NetId, ProjectileId, PropId};

pub use crate::weapons::WeaponSlot;

#[derive(Clone, Debug, PartialEq)]
pub enum SimEvent {
    /// A weapon fired. `projectile` dedupes repeated unreliable `FireEvent`s.
    Fired {
        shooter: NetId,
        projectile: ProjectileId,
        slot: WeaponSlot,
        muzzle: Vec3,
        dir: Vec3,
        tick: u32,
        /// High-velocity rounds are validated through hitbox-history rewind by the server.
        hitscan: bool,
    },
    ProjectileHit {
        projectile: ProjectileId,
        target: NetId,
        position: Vec3,
    },
    ProjectileExpire {
        projectile: ProjectileId,
        position: Vec3,
    },
    Damaged {
        target: NetId,
        attacker: NetId,
        amount: f32,
        recoil: f32,
        /// `acvparts.bin` damage attribute byte (0 KE, 1 TE, 6 CE, other = unmitigated).
        attribute: u8,
        effective: bool,
        lethal: bool,
        position: Vec3,
        tick: u32,
    },
    Killed {
        target: NetId,
        attacker: NetId,
        tick: u32,
    },
    PropBroken {
        prop: PropId,
        tick: u32,
    },
    Respawned {
        target: NetId,
        position: Vec3,
        yaw: f32,
        tick: u32,
    },
}
