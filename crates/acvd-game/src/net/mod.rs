//! Multiplayer client adapter.
//!
//! Solo play stays on the direct `LocalSession` path: when no `NetRuntime` resource exists,
//! `control::pilot` steps the AC exactly as before and none of these systems run. Online play
//! samples inputs at 60 Hz, predicts locally, reconciles against `LocalPlayerState`, interpolates
//! remote ACs 4 ticks behind, and spawns real assembled rigs per `LoadoutUpdate`.

pub mod combat;
pub mod input_sampler;
pub mod interpolation;
pub mod prediction;
pub mod session;
pub mod view;

pub use session::{NetConfig, NetInput, NetRuntime};
pub use view::NetView;

use bevy::prelude::*;

/// Run condition for systems that must not run during an online match (client-authoritative
/// single-player fire/melee/health/projectile stepping).
pub fn solo_play(runtime: Option<Res<NetRuntime>>) -> bool {
    runtime.is_none()
}

/// Registers the online systems. The resource is inserted only in network mode, so solo play is
/// untouched.
pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NetView>()
            .init_resource::<combat::NetFxState>()
            .add_systems(
                Update,
                (
                    session::update_transport,
                    // DestructibleBreak deltas land in collision before reconciliation replay.
                    session::apply_world_deltas,
                    combat::apply_combat,
                    session::step_prediction,
                    combat::spawn_fire_events,
                    crate::weapons::step_net_projectiles,
                    view::sync,
                )
                    .chain()
                    .after(crate::control::pilot)
                    .before(crate::pose::animate)
                    .run_if(resource_exists::<NetRuntime>),
            );
    }
}
