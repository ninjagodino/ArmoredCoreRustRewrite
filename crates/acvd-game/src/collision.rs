//! Bevy resource wrapper around `acvd_sim::collision`.
//!
//! The collision data/queries are pure simulation code in `acvd-sim`; Bevy only needs a
//! `Resource` around it. `Collision` is an alias so existing systems keep their type name, and
//! `Deref`/`DerefMut` forward every query to the sim type (including coercion at call sites that
//! take `&acvd_sim::collision::Collision`).

use std::ops::{Deref, DerefMut};

use acvd_formats::vfs::Disc;
use anyhow::Result;
use bevy::prelude::Resource;

pub use acvd_sim::collision::Collision as SimCollision;
pub use acvd_sim::collision::{GroundSurface, Hit, Layer, Plane, RayFilter, RAY_HALF};

/// Bevy wrapper around the pure simulation collision world.
#[derive(Resource, Default)]
pub struct CollisionRes(pub SimCollision);

/// Existing game code keeps using `Collision` as the resource type name.
pub type Collision = CollisionRes;

impl Deref for CollisionRes {
    type Target = SimCollision;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for CollisionRes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl CollisionRes {
    pub fn with_hits(planes: Vec<Plane>, hits: Vec<Hit>) -> Self {
        Self(SimCollision::with_hits(planes, hits))
    }
}

/// Loads the map's `.hmd` collision into the Bevy resource wrapper.
pub fn load_map(disc: &Disc, map: &str) -> Result<Collision> {
    acvd_sim::collision::load_map(disc, map).map(CollisionRes)
}
