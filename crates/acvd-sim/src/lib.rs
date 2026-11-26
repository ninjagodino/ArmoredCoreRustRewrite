//! Pure, GPU-free Armored Core simulation.
//!
//! This crate owns the fixed 60 Hz game state: movement (`motion`), map collision (`collision`),
//! the damage model (`damage`), destructible map objects (`map`), projectile/melee state
//! (`weapons`), and the world tick (`world`). It never depends on Bevy, a window, audio, assets,
//! or the wall clock; `acvd-game` and the future `acvd-server` call the same `step_tick`.
//!
//! Phase 0 of the netcode plan only fills the movement/collision/damage/destructible subset and
//! leaves `weapons`/`world`/`stats` as skeletons for the later phases.

pub mod collision;
pub mod damage;
pub mod events;
pub mod ids;
pub mod input;
pub mod map;
pub mod motion;
pub mod rng;
pub mod stats;
pub mod tick;
pub mod weapons;
pub mod world;
