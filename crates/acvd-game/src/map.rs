//! Map scene: the visual FLVERs of a map's MSB parts, and the start point of its layout.
//!
//! A map folder `model/map/{map}` holds `{map}_map.msb` (terrain and objects), optional layout
//! MSBs such as `{map}_actest.msb` (the garage AC test: start points, areas, test enemies, no
//! terrain), `{map}_m.dcx.bnd` (map piece `m####.flv` / `_h.hmd`) and `{map}_htdcx.bnd` (one
//! `{texture}.tpf.dcx` per texture; `{map}_l.tpf.dcx` is the low-resolution copy). Objects
//! (part kind 1) load from `model/obj/{o}/{o}_m.bnd.dcx`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use acvd_data::find;
use acvd_data::generated::ac_unit::PARAM_ACOBJECTPARAM_BIN;
use acvd_data::generated::enemy::PARAM_DESTROYAP_PARAM;
use acvd_data::generated::map::PARAM_MAPOBJBEHAVIORPARAM_BIN;
use acvd_data::generated::sfx::PARAM_SFXSESETPARAM_BIN;
use acvd_data::generated::sound::PARAM_ACSOUNDPARAM_BIN;
use acvd_formats::vfs::{self, Disc};
use acvd_formats::{bnd3, env as fmt_env, flver, mlb, msb, nav, tae};
use acvd_render::{LoadedMesh, Packs};
use anyhow::Result;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::math::Affine3A;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::prelude::*;

use crate::collision::Collision;
use crate::sfx;
use crate::sound;
use crate::targets::{self, EnemyAnim, EnemyAnimBank};

/// Map part kinds that have a visual model and a hit model.
const KIND_PIECE: u32 = 0;
pub(crate) const KIND_OBJECT: u32 = 1;

pub fn folder(map: &str) -> String {
    format!("model/map/{map}")
}

/// `{map}_map.msb`, else `{map}.msb`.
pub fn terrain_msb(disc: &Disc, map: &str) -> String {
    let named = format!("{}/{map}_map.msb", folder(map));
    if disc.exists(&named) {
        named
    } else {
        format!("{}/{map}.msb", folder(map))
    }
}

/// Binder asset holding `part`'s models, or `None` for kinds without map geometry.
pub fn binder(map: &str, part: &msb::Part) -> Option<String> {
    match part.kind {
        KIND_PIECE => Some(format!("{}/{map}_m.dcx.bnd", folder(map))),
        KIND_OBJECT => Some(format!("model/obj/{0}/{0}_m.bnd.dcx", part.model)),
        _ => None,
    }
}

/// TPF holding `part`'s textures, or `None` for map pieces whose textures live as
/// `{name}.tpf.dcx` members of `{map}_htdcx.bnd`. Object models keep their textures in their own
/// `model/obj/{model}/{model}.tpf.dcx`; many objects share texture names (`net`, `o2036`, ...) with
/// other maps, so a name-only global lookup picks an unrelated, brighter pack.
pub fn texture_pack(disc: &Disc, part: &msb::Part) -> Option<String> {
    if part.kind != KIND_OBJECT {
        return None;
    }
    let pack = format!("model/obj/{0}/{0}.tpf.dcx", part.model);
    disc.exists(&pack).then_some(pack)
}

/// The part's placement in FLVER (game) axes.
pub fn xform(part: &msb::Part) -> flver::Xform {
    flver::Xform::local(
        part.translation,
        part.rotation_deg.map(f32::to_radians),
        part.scale,
    )
}

/// Where the player AC starts: Bevy position and yaw (radians, as `control::Pilot::yaw`).
#[derive(Resource, Clone, Copy, Debug)]
pub struct Start {
    pub position: Vec3,
    pub yaw: f32,
}

/// Water-return respawn points (`POINT_PARAM_ST` kind `50`, `msb::POINT_WATER_RETURN`) and
/// water surface height (`env::Scene.water_height` from `model/map/ch_env/{map}_env.msb`
/// type-101 sub-block 0 `+0x08`, 360 `0x827ffa80`, overridden by `--water <y>`).
#[derive(Resource, Clone, Debug, Default)]
pub struct WaterReturnPoints {
    pub points: Vec<Start>,
    pub water_height: Option<f32>,
}

impl WaterReturnPoints {
    pub fn from_points(points: &[msb::Point], water_height: Option<f32>) -> Self {
        let pts = points
            .iter()
            .filter(|p| p.kind == msb::POINT_WATER_RETURN)
            .map(|p| {
                let [x, y, z] = p.translation;
                Start {
                    position: Vec3::new(-x, y, z),
                    yaw: -p.rotation_deg[1].to_radians(),
                }
            })
            .collect();
        Self {
            points: pts,
            water_height,
        }
    }

    /// Loads kind-50 water-return points from `{map}_{layout}.msb` and `{map}_map.msb`, plus
    /// `water_height` from `model/map/ch_env/{map}_env.msb` (or `water_override` if set).
    pub fn load(
        disc: &Disc,
        map: &str,
        layout: Option<&str>,
        water_override: Option<f32>,
    ) -> Result<Self> {
        let mut sources = Vec::new();
        if let Some(l) = layout {
            sources.push(format!("{}/{map}_{l}.msb", folder(map)));
        }
        sources.push(terrain_msb(disc, map));
        let mut all_points = Vec::new();
        for path in sources.into_iter().filter(|p| disc.exists(p)) {
            let pts = msb::points(&vfs::open(disc, &path)?)?;
            all_points.extend(
                pts.into_iter()
                    .filter(|p| p.kind == msb::POINT_WATER_RETURN),
            );
        }
        let water_height = water_override.or_else(|| {
            let env_path = format!("model/map/ch_env/{map}_env.msb");
            vfs::open(disc, &env_path)
                .ok()
                .and_then(|d| fmt_env::read(&d).ok())
                .and_then(|e| e.scene)
                .map(|s| s.water_height)
        });
        Ok(Self::from_points(&all_points, water_height))
    }

    /// Returns the water-return point closest to `pos` in XZ, or `fallback` if none exist.
    pub fn nearest(&self, pos: Vec3, fallback: Start) -> Start {
        self.points
            .iter()
            .copied()
            .min_by(|a, b| {
                let da = Vec2::new(a.position.x - pos.x, a.position.z - pos.z).length_squared();
                let db = Vec2::new(b.position.x - pos.x, b.position.z - pos.z).length_squared();
                da.total_cmp(&db)
            })
            .unwrap_or(fallback)
    }
}

/// Destructible map object (`KIND_OBJECT` whose binder ships `{model}_b.flv`, 360 `0x82c02498`,
/// `DestroyModel` `0x82c84a60`, durability from `{model}_m.mlb` -> `PARAM_DESTROYAP_PARAM`
/// `0x82c01de8` / `0x82c01bd8`).
#[derive(Component, Debug)]
pub struct DestructibleProp {
    /// Matches `collision::Hit::prop_id` (`part_idx + 1`).
    pub prop_id: u32,
    pub model: String,
    /// World-space bounding sphere center and radius (for AOE / melee / kick hits).
    pub center: Vec3,
    pub radius: f32,
    /// Remaining HP before breaking into `_b.flv` / `_b_h.hmd`.
    pub hp: f32,
    pub max_hp: f32,
    /// Minimum damage threshold (`DestroyApSt.cutoff_damage`, 360 `0x829ad0f8`; `65535` = indestructible).
    pub cutoff_damage: f32,
    /// AC-body collision damage multiplier (`0x82c01bd8`: `(100 - collision_defensibility) * 0.01 * collision_damage_coef`).
    /// `0.0` = immune to AC walking/sliding/boosting collision (`collision_defensibility == 100`).
    pub collision_damage_mult: f32,
    /// Nonzero `AcObjectParam.damage_id` (e.g. explosive fuel tanks `89110..89113`).
    pub damage_id: i32,
    pub full: Entity,
    pub low: Option<Entity>,
    pub broken_mesh: Entity,
    pub broken: bool,
    /// Set on the lethal hit; consumed by [`step_destructibles`].
    pub just_broken: bool,
}

impl DestructibleProp {
    /// Whether this prop can be broken by AC body collision (`0x829aabe8`: `mult >= 0.001`).
    pub fn is_collision_destructible(&self) -> bool {
        !self.broken && self.collision_damage_mult >= 0.001 && self.cutoff_damage < 65535.0
    }

    /// Applies weapon/explosion `damage` to this prop (`0x829ad0f8`); returns `true` on the hit
    /// that breaks it.
    pub fn apply_damage(&mut self, damage: f32) -> bool {
        if self.broken || damage <= 0.0 || damage <= self.cutoff_damage {
            return false;
        }
        self.hp = (self.hp - damage).max(0.0);
        if self.hp <= 0.0 {
            self.broken = true;
            self.just_broken = true;
            true
        } else {
            false
        }
    }

    /// Applies AC body collision damage (`damage_type == 2`, 360 `0x82865470` -> `0x829ad0f8`),
    /// scaling `raw_damage` by `collision_damage_mult` (`0x82c01bd8`). Returns `true` if this
    /// contact breaks the prop.
    pub fn apply_collision_damage(&mut self, raw_damage: f32) -> bool {
        if !self.is_collision_destructible() || raw_damage <= 0.0 {
            return false;
        }
        self.apply_damage(raw_damage * self.collision_damage_mult)
    }
}

/// One MSB `POINT_PARAM_ST` area box (kinds `100` operation / `101` warning / `102` caution,
/// `shape == 5`, `m4000_actest.msb`), already mirrored on X into Bevy coordinates.
#[derive(Clone, Copy, Debug)]
pub struct AreaBox {
    /// Center X/Z and bottom Y in Bevy world metres (`translation`, X mirrored).
    pub origin: Vec3,
    /// Half-width along local X (`width * 0.5`).
    pub half_x: f32,
    /// Half-depth along local Z (`depth * 0.5`).
    pub half_z: f32,
    /// Height above `origin.y` (`height`).
    pub height: f32,
    /// Yaw in Bevy radians (`-rotation_deg[1].to_radians()`).
    pub yaw: f32,
}

impl AreaBox {
    pub fn from_point(p: &msb::Point) -> Option<Self> {
        let msb::PointShape::Box {
            width,
            depth,
            height,
        } = p.shape_data
        else {
            return None;
        };
        let [x, y, z] = p.translation;
        Some(Self {
            origin: Vec3::new(-x, y, z),
            half_x: (width * 0.5).max(1.0),
            half_z: (depth * 0.5).max(1.0),
            height: height.max(1.0),
            yaw: -p.rotation_deg[1].to_radians(),
        })
    }

    fn to_local(&self, pos: Vec3) -> Vec3 {
        let rel = pos - self.origin;
        if self.yaw.abs() < 1e-6 {
            rel
        } else {
            Quat::from_rotation_y(-self.yaw) * rel
        }
    }

    fn from_local(&self, local: Vec3) -> Vec3 {
        let rot = if self.yaw.abs() < 1e-6 {
            local
        } else {
            Quat::from_rotation_y(self.yaw) * local
        };
        self.origin + rot
    }

    /// True if `pos` lies inside this area box (`|lx| <= half_x`, `|lz| <= half_z`, `0 <= ly <= height`).
    pub fn contains(&self, pos: Vec3) -> bool {
        let l = self.to_local(pos);
        l.x.abs() <= self.half_x && l.z.abs() <= self.half_z && l.y >= 0.0 && l.y <= self.height
    }

    /// Clamps `pos` inside this box (with optional inward `margin`) and zeroes any outward
    /// velocity component in `vel`. Returns `true` if `pos` was clamped.
    pub fn clamp(&self, pos: &mut Vec3, vel: &mut Vec3, margin: f32) -> bool {
        let hx = (self.half_x - margin).max(1.0);
        let hz = (self.half_z - margin).max(1.0);
        let h = (self.height - margin).max(1.0);
        let mut l = self.to_local(*pos);
        let mut lv = if self.yaw.abs() < 1e-6 {
            *vel
        } else {
            Quat::from_rotation_y(-self.yaw) * *vel
        };
        let mut clamped = false;
        if l.x < -hx {
            l.x = -hx;
            if lv.x < 0.0 {
                lv.x = 0.0;
            }
            clamped = true;
        } else if l.x > hx {
            l.x = hx;
            if lv.x > 0.0 {
                lv.x = 0.0;
            }
            clamped = true;
        }
        if l.z < -hz {
            l.z = -hz;
            if lv.z < 0.0 {
                lv.z = 0.0;
            }
            clamped = true;
        } else if l.z > hz {
            l.z = hz;
            if lv.z > 0.0 {
                lv.z = 0.0;
            }
            clamped = true;
        }
        if l.y > h {
            l.y = h;
            if lv.y > 0.0 {
                lv.y = 0.0;
            }
            clamped = true;
        }
        if clamped {
            *pos = self.from_local(l);
            *vel = if self.yaw.abs() < 1e-6 {
                lv
            } else {
                Quat::from_rotation_y(self.yaw) * lv
            };
        }
        clamped
    }
}

/// Which map area band the AC currently occupies (`POINT_PARAM_ST` kinds `100`–`102`,
/// `manual.fmg` 20500 `LIMIT OF AREA` / `OUTSIDE OF AREA`, `AreaOverLine` `0x825b3678`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AreaZone {
    /// Inside the inner caution area (`kind == 102`) or no area bounds defined.
    #[default]
    Inside,
    /// Outside caution area (`102`), inside warning area (`101`): `LIMIT OF AREA`.
    Caution,
    /// Outside warning area (`101`), inside operation area (`100`): `LIMIT OF AREA` (urgent).
    Warning,
    /// At or outside the operation area (`100`): `OUTSIDE OF AREA` (clamped).
    Outside,
}

/// Map operation / warning / caution area boxes from `{map}_{layout}.msb` (`POINT_PARAM_ST`
/// kinds `100`, `101`, `102`).
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct AreaBounds {
    pub operation: Option<AreaBox>,
    pub warning: Option<AreaBox>,
    pub caution: Option<AreaBox>,
}

/// Loaded `HNAV` + `HTR` navigation mesh for the active map (`model/map/ch_nav/{map}.hnav`,
/// `*.htr`), with coordinate conversion between Bevy world space and FLVER/game axes (`x -> -x`).
#[derive(Resource, Clone, Debug, Default)]
pub struct MapNav {
    inner: Option<nav::NavMesh>,
}

impl MapNav {
    /// Loads `model/map/ch_nav/{map}.hnav` (and optional `.htr`) from the disc.
    pub fn load(disc: &Disc, map: &str) -> Result<Self> {
        let hnav_path = format!("model/map/ch_nav/{map}.hnav");
        if !disc.exists(&hnav_path) {
            return Ok(Self { inner: None });
        }
        let hnav = nav::read_hnav(&vfs::open(disc, &hnav_path)?)?;
        let htr_path = format!("model/map/ch_nav/{map}.htr");
        let htr = if disc.exists(&htr_path) {
            Some(nav::read_htr(&vfs::open(disc, &htr_path)?)?)
        } else {
            None
        };
        Ok(Self {
            inner: Some(nav::NavMesh::new(hnav, htr)),
        })
    }

    pub fn is_loaded(&self) -> bool {
        self.inner.is_some()
    }

    pub fn region_count(&self) -> usize {
        self.inner.as_ref().map_or(0, nav::NavMesh::region_count)
    }

    /// Computes a Bevy-world waypoint path from `start` to `goal` over the `HNAV` navmesh.
    pub fn find_path(&self, start: Vec3, goal: Vec3) -> Vec<Vec3> {
        let Some(mesh) = &self.inner else {
            return Vec::new();
        };
        let s = [-start.x, start.y, start.z];
        let g = [-goal.x, goal.y, goal.z];
        mesh.find_path(s, g)
            .into_iter()
            .map(|p| Vec3::new(-p[0], p[1], p[2]))
            .collect()
    }
}

impl AreaBounds {
    pub fn from_points(points: &[msb::Point]) -> Self {
        let find = |kind: u32| {
            points
                .iter()
                .find(|p| p.kind == kind)
                .and_then(AreaBox::from_point)
        };
        Self {
            operation: find(msb::POINT_OPERATION_AREA),
            warning: find(msb::POINT_WARNING_AREA),
            caution: find(msb::POINT_CAUTION_AREA),
        }
    }

    /// Loads area bounds (`POINT` kinds `100`–`102`) from `{map}_{layout}.msb`, falling back to
    /// the terrain MSB.
    pub fn load(disc: &Disc, map: &str, layout: Option<&str>) -> Result<Self> {
        let mut sources = Vec::new();
        if let Some(l) = layout {
            sources.push(format!("{}/{map}_{l}.msb", folder(map)));
        }
        sources.push(terrain_msb(disc, map));
        for path in sources.into_iter().filter(|p| disc.exists(p)) {
            let pts = msb::points(&vfs::open(disc, &path)?)?;
            let bounds = Self::from_points(&pts);
            if bounds.operation.is_some() || bounds.warning.is_some() || bounds.caution.is_some() {
                return Ok(bounds);
            }
        }
        Ok(Self::default())
    }

    /// Classifies `pos` into `Inside`, `Caution`, `Warning`, or `Outside`.
    pub fn zone(&self, pos: Vec3) -> AreaZone {
        if let Some(op) = self.operation {
            if !op.contains(pos) {
                return AreaZone::Outside;
            }
        }
        if let Some(warn) = self.warning {
            if !warn.contains(pos) {
                return AreaZone::Warning;
            }
        }
        if let Some(caut) = self.caution {
            if !caut.contains(pos) {
                return AreaZone::Caution;
            }
        }
        AreaZone::Inside
    }

    /// Clamps `pos` and outward velocity `vel` inside the operation area (`kind == 100`) and
    /// returns the resulting [`AreaZone`] (`AreaZone::Outside` when clamped at the boundary).
    pub fn enforce(&self, pos: &mut Vec3, vel: &mut Vec3) -> AreaZone {
        let clamped = self.operation.is_some_and(|op| op.clamp(pos, vel, 1.0));
        if clamped {
            AreaZone::Outside
        } else {
            self.zone(*pos)
        }
    }
}

/// Start point index 0 (`msb::POINT_START`) of `{map}_{layout}.msb`, else of the terrain MSB.
/// MSB axes mirror X into Bevy, which also negates a yaw.
pub fn start(disc: &Disc, map: &str, layout: Option<&str>) -> Result<Option<Start>> {
    let mut sources = Vec::new();
    if let Some(l) = layout {
        sources.push(format!("{}/{map}_{l}.msb", folder(map)));
    }
    sources.push(terrain_msb(disc, map));
    for path in sources.into_iter().filter(|p| disc.exists(p)) {
        let points = msb::points(&vfs::open(disc, &path)?)?;
        if let Some(p) = points
            .iter()
            .find(|p| p.kind == msb::POINT_START && p.kind_index == 0)
        {
            let [x, y, z] = p.translation;
            return Ok(Some(Start {
                position: Vec3::new(-x, y, z),
                yaw: -p.rotation_deg[1].to_radians(),
            }));
        }
    }
    Ok(None)
}

/// A map's drawn parts; children carry the meshes.
#[derive(Component)]
pub struct MapPart;

/// Per-submesh FLVER material metadata passed from [`spawn`] to `env::relight`
/// (`Map_Diffuse_Multi` secondary diffuse and `Water.spx` detection).
#[derive(Component, Clone, Debug)]
pub struct MapMeshMeta {
    pub mtd: String,
    pub diffuse2: Option<Handle<Image>>,
}

/// One placed part's two meshes. The full mesh is `{model}.flv`; `low` is `{model}_l1.flv`
/// when the binder has one (`sheets/map_lod.csv`).
#[derive(Component)]
pub(crate) struct MapLod {
    /// Bounding-sphere centre, world metres.
    center: Vec3,
    radius: f32,
    full: Entity,
    low: Option<Entity>,
}

/// LOD metric `max(distance - radius, 0.1) / (2 * radius)` (`0x82d13e70`, `0x82d107f0`).
/// Level 0 below 0.1, level 1 below 0.4, level 2 below 2, level 3 through 15, hidden past 15
/// (`0x82d10640`). Map pieces only ship level 0 and `_l1`, so levels 1-3 share that mesh.
fn lod_metric(distance: f32, radius: f32) -> f32 {
    (distance - radius).max(0.1) / (2.0 * radius)
}

fn sphere(xf: &Transform, min: Vec3, max: Vec3) -> (Vec3, f32) {
    let center = (min + max) * 0.5;
    let extent = (max - min) * 0.5;
    let world = xf.transform_point(center);
    let mut radius = 0.5f32;
    for (sx, sy, sz) in [
        (1.0, 1.0, 1.0),
        (1.0, 1.0, -1.0),
        (1.0, -1.0, 1.0),
        (1.0, -1.0, -1.0),
        (-1.0, 1.0, 1.0),
        (-1.0, 1.0, -1.0),
        (-1.0, -1.0, 1.0),
        (-1.0, -1.0, -1.0),
    ] {
        let corner =
            xf.transform_point(center + Vec3::new(extent.x * sx, extent.y * sy, extent.z * sz));
        radius = radius.max(world.distance(corner));
    }
    (world, radius)
}

fn mesh_bounds(list: &[Loaded]) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for (_, _, _, a, b) in list {
        min = min.min(*a);
        max = max.max(*b);
    }
    (min, max)
}

/// Spawns every map piece and object of `map`'s terrain MSB. Returns how many parts drew.
/// A part whose binder also has `{model}_l1.flv` keeps that mesh for the lower LOD levels.
pub fn spawn(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
    disc: &Disc,
    map: &str,
) -> Result<usize> {
    let parts = msb::parts(&vfs::open(disc, &terrain_msb(disc, map))?)?;
    let mut models: HashMap<String, Option<Vec<Loaded>>> = HashMap::new();
    let mut animated_models: HashMap<String, Option<AnimatedModel>> = HashMap::new();
    let mut members: HashMap<String, HashSet<String>> = HashMap::new();
    let mut textures = Textures {
        own: format!("{}/{map}_htdcx.bnd", folder(map)),
        ..default()
    };
    let mut drawn = 0;
    let mut low_parts = 0;
    let mut destructible_parts = 0;
    let mut animated_parts = 0;
    let mut destroy_ap_cache: HashMap<String, Option<u16>> = HashMap::new();
    let mut object_fx: HashMap<String, Vec<(i32, [f32; 3], [f32; 3])>> = HashMap::new();
    let mut object_fx_spawned = 0usize;
    for (part_idx, part) in parts.iter().enumerate() {
        let Some(binder) = binder(map, part) else {
            continue;
        };
        // Aircraft the map itself places. The part's `+0x44` behavior id selects a
        // `MAPOBJ_PARAM_ST` row; its `Mob_DisperseTAE` names the TAE timeline, whose
        // kind-104 event names the ANI clip in the model's `_a` binder. No traffic is added.
        if part.kind == KIND_OBJECT && is_aircraft_model(&part.model) {
            let pack = texture_pack(disc, part);
            let pack = pack.as_deref();
            if let Some(loaded) = load_animated_model(
                &mut animated_models,
                &mut textures,
                meshes,
                materials,
                images,
                bindposes,
                disc,
                &part.model,
                pack,
            ) {
                if let Some((clip, speed, looping)) =
                    aircraft_clip(&loaded.bank, loaded.tae.as_ref(), part)
                {
                    drawn += 1;
                    animated_parts += 1;
                    let root = commands
                        .spawn((
                            MapPart,
                            bevy_transform(&xform(part)),
                            Visibility::default(),
                            Name::new(part.name.clone()),
                        ))
                        .id();
                    spawn_animated_part(commands, root, &loaded, clip, speed, looping);
                    continue;
                }
            }
        }
        let asset = format!("{binder}|{}.flv", part.model);
        let pack = texture_pack(disc, part);
        let pack = pack.as_deref();
        let Some(list) = load_model(
            &mut models,
            &mut textures,
            meshes,
            materials,
            images,
            disc,
            &asset,
            pack,
        ) else {
            continue;
        };
        if list.is_empty() {
            continue;
        }
        let low_file = format!("{}_l1.flv", part.model);
        let low = if binder_has(&mut members, disc, &binder, &low_file) {
            low_parts += 1;
            load_model(
                &mut models,
                &mut textures,
                meshes,
                materials,
                images,
                disc,
                &format!("{binder}|{low_file}"),
                pack,
            )
        } else {
            None
        };
        let broken_file = format!("{}_b.flv", part.model);
        let broken =
            if part.kind == KIND_OBJECT && binder_has(&mut members, disc, &binder, &broken_file) {
                load_model(
                    &mut models,
                    &mut textures,
                    meshes,
                    materials,
                    images,
                    disc,
                    &format!("{binder}|{broken_file}"),
                    pack,
                )
            } else {
                None
            };
        drawn += 1;
        let xf = bevy_transform(&xform(part));
        let (min, max) = mesh_bounds(&list);
        let (center, radius) = sphere(&xf, min, max);
        let mut full = Entity::PLACEHOLDER;
        let mut low_entity = None;
        let mut broken_entity = None;
        let parent = commands
            .spawn((
                MapPart,
                xf,
                Visibility::default(),
                Name::new(part.name.clone()),
            ))
            .id();
        let effects = if part.kind == KIND_OBJECT {
            object_fx
                .entry(part.model.clone())
                .or_insert_with(|| {
                    object_effects(disc, &part.model, &format!("{binder}|{}.flv", part.model))
                })
                .clone()
        } else {
            Vec::new()
        };
        commands.entity(parent).with_children(|c| {
            full = spawn_lod(c, &list, Visibility::Inherited);
            if let Some(low) = &low {
                low_entity = Some(spawn_lod(c, low, Visibility::Hidden));
            }
            if let Some(broken) = &broken {
                broken_entity = Some(spawn_lod(c, broken, Visibility::Hidden));
            }
            for (ffx_id, position, forward) in effects {
                spawn_object_effect(c, ffx_id, position, forward);
                object_fx_spawned += 1;
            }
        });
        commands.entity(parent).insert(MapLod {
            center,
            radius,
            full,
            low: low_entity,
        });
        if let Some(broken_mesh) = broken_entity {
            destructible_parts += 1;
            let obj_id = part
                .model
                .strip_prefix('o')
                .or_else(|| part.model.strip_prefix('O'))
                .and_then(|s| s.parse::<u32>().ok());
            let damage_id = obj_id
                .and_then(|id| find(PARAM_ACOBJECTPARAM_BIN, id))
                .map_or(0, |r| r.data.damage_id);
            let destroy_ap_id = *destroy_ap_cache
                .entry(part.model.clone())
                .or_insert_with(|| {
                    let mlb_path = format!("{binder}|{}_m.mlb", part.model);
                    vfs::open(disc, &mlb_path)
                        .ok()
                        .and_then(|b| mlb::read_destroy_ap_ids(&b).ok())
                        .and_then(|ids| ids.primary_id())
                });
            let (max_hp, cutoff_damage, collision_damage_mult) = destroy_ap_id
                .and_then(|id| find(PARAM_DESTROYAP_PARAM, u32::from(id)))
                .map(|r| {
                    let d = &r.data;
                    // 360 `0x82c01bd8`: (100 - collision_defensibility) * 0.01 * collision_damage_coef
                    let mult = f32::from(100u8.saturating_sub(d.collision_defensibility))
                        * 0.01
                        * d.collision_damage_coef;
                    (d.ap.max(1) as f32, d.cutoff_damage as f32, mult.max(0.0))
                })
                .unwrap_or_else(|| {
                    let hp = if damage_id > 0 { 800.0 } else { 500.0 };
                    (hp, 0.0, 1.0)
                });
            commands.entity(parent).insert(DestructibleProp {
                prop_id: (part_idx + 1) as u32,
                model: part.model.clone(),
                center,
                radius: radius.max(2.0),
                hp: max_hp,
                max_hp,
                cutoff_damage,
                collision_damage_mult,
                damage_id,
                full,
                low: low_entity,
                broken_mesh,
                broken: false,
                just_broken: false,
            });
        }
    }
    if textures.missing > 0 {
        warn!("map {map}: {} textures missing", textures.missing);
    }
    info!(
        "map {map}: {drawn} parts, {low_parts} with an _l1 mesh, {destructible_parts} destructible (_b.flv), {object_fx_spawned} object FFX, {animated_parts} animated aircraft"
    );
    Ok(drawn)
}

type Loaded = (
    Handle<Mesh>,
    Handle<StandardMaterial>,
    MapMeshMeta,
    Vec3,
    Vec3,
);

/// Aircraft-like map-object families (`o92xx`..`o96xx`). Their placements come from the
/// map's PARTS section; the rewrite only animates those parts and never spawns new traffic.
fn is_aircraft_model(model: &str) -> bool {
    let b = model.as_bytes();
    b.len() >= 3 && b[0] == b'o' && b[1] == b'9' && matches!(b[2], b'2'..=b'6')
}

/// Marks the root of a map-placed aircraft formation so its flight clip can be stepped
/// independently of the AI-driven enemies.
#[derive(Component)]
pub struct MapAircraft;

/// Advances the rotor/flight clips of map-placed aircraft. They carry no AI component, so
/// `ai::update` never steps their [`EnemyAnim`] clocks.
pub fn step_aircraft(time: Res<Time>, mut aircraft: Query<&mut EnemyAnim, With<MapAircraft>>) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    for mut anim in &mut aircraft {
        anim.step(dt, None);
    }
}

/// One animated map-object model shared by every placement of it in the map.
#[derive(Clone)]
struct AnimatedModel {
    meshes: Vec<(Handle<Mesh>, Handle<StandardMaterial>, MapMeshMeta)>,
    inv_bindposes: Handle<SkinnedMeshInverseBindposes>,
    bank: Arc<EnemyAnimBank>,
    tae: Option<tae::Tae>,
}

/// The TAE id and loop flag selected by a part's `+0x44` behavior row.
fn behavior_tae(part: &msb::Part) -> Option<(u32, bool)> {
    let id: u32 = part.behavior.as_deref()?.parse().ok()?;
    let row = find(PARAM_MAPOBJBEHAVIORPARAM_BIN, id)?;
    let data = &row.data;
    let tae = [
        data.mob_disperse_tae,
        data.mob_disperse_loop_tae,
        data.mob_regroup_tae,
        data.mob_regroup_loop_tae,
    ]
    .into_iter()
    .find(|&t| t != 0)?;
    Some((u32::from(tae), matches!(data.behavior_type, 4 | 7)))
}

/// Resolves the ANI clip a map part plays: behavior row → `Mob_DisperseTAE` timeline →
/// its kind-104 event (`[ani_id, flag, 0, 0, speed]`).
fn aircraft_clip(
    bank: &EnemyAnimBank,
    tae: Option<&tae::Tae>,
    part: &msb::Part,
) -> Option<(u16, f32, bool)> {
    let (tae_id, looping) = behavior_tae(part)?;
    let anim = tae?.animations.iter().find(|a| a.id == tae_id)?;
    let event = anim
        .events
        .iter()
        .find(|e| e.kind == tae::KIND_ANI && e.params.len() >= 20)?;
    let clip = u16::try_from(u32::from_be_bytes(event.params[0..4].try_into().ok()?)).ok()?;
    let speed = f32::from_bits(u32::from_be_bytes(event.params[16..20].try_into().ok()?));
    let speed = if speed.is_finite() && speed > 0.0 {
        speed
    } else {
        1.0
    };
    bank.clips
        .contains_key(&clip)
        .then_some((clip, speed, looping))
}

#[allow(clippy::too_many_arguments)]
fn load_animated_model(
    cache: &mut HashMap<String, Option<AnimatedModel>>,
    textures: &mut Textures,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
    disc: &Disc,
    model: &str,
    pack: Option<&str>,
) -> Option<AnimatedModel> {
    if let Some(cached) = cache.get(model) {
        return cached.clone();
    }
    let binder = format!("model/obj/{model}/{model}_a.bnd.dcx");
    let tae_asset = format!("tae/obj/{model}/own000100.tae");
    let loaded = if !disc.exists(&binder) || !disc.exists(&tae_asset) {
        None
    } else {
        let asset = format!("model/obj/{model}/{model}_m.bnd.dcx|{model}.flv");
        match acvd_render::rigged_model(disc, &asset, &|_| acvd_render::RootPlace::IDENTITY) {
            Ok((list, rig)) if !list.is_empty() => {
                let bank = EnemyAnimBank::load_object(disc, model, &rig);
                if bank.clips.is_empty() {
                    None
                } else {
                    let unboned = Affine3A::from_translation(Vec3::from(rig.unboned));
                    let binds: Vec<Mat4> = rig
                        .bones
                        .iter()
                        .map(|b| targets::xform_to_affine(&b.bind))
                        .chain([unboned])
                        .map(|b| Mat4::from(crate::pose::mirror(b).inverse()))
                        .collect();
                    let inv_bindposes = bindposes.add(SkinnedMeshInverseBindposes::from(binds));
                    let meshes_mats = list
                        .into_iter()
                        .map(|m| {
                            let material = textures.material(disc, pack, &m, materials, images);
                            let meta = MapMeshMeta {
                                mtd: m.mtd.clone(),
                                diffuse2: m.diffuse2.as_deref().and_then(|n| {
                                    textures.image_handle(disc, pack, n, false, images)
                                }),
                            };
                            (meshes.add(m.mesh), material, meta)
                        })
                        .collect();
                    let tae = vfs::open(disc, &tae_asset)
                        .ok()
                        .and_then(|bytes| tae::read(&bytes).ok());
                    Some(AnimatedModel {
                        meshes: meshes_mats,
                        inv_bindposes,
                        bank,
                        tae,
                    })
                }
            }
            Ok(_) => None,
            Err(e) => {
                warn!("{asset}: {e:#}");
                None
            }
        }
    };
    cache.insert(model.to_string(), loaded.clone());
    loaded
}

/// Spawns the skinned meshes and joint hierarchy under a map part's world placement.
fn spawn_animated_part(
    commands: &mut Commands,
    root: Entity,
    loaded: &AnimatedModel,
    clip: u16,
    speed: f32,
    looping: bool,
) {
    let mut joint_entities: Vec<Entity> = Vec::with_capacity(loaded.bank.rest_locals.len());
    for _ in 0..loaded.bank.rest_locals.len() {
        joint_entities.push(
            commands
                .spawn((Transform::default(), Visibility::default()))
                .id(),
        );
    }
    for (i, &ent) in joint_entities.iter().enumerate() {
        let parent_ent = loaded.bank.parents[i].map_or(root, |p| joint_entities[p]);
        commands
            .entity(ent)
            .insert((loaded.bank.rest_locals[i], ChildOf(parent_ent)));
    }
    let extra_unboned = commands
        .spawn((Transform::default(), Visibility::default(), ChildOf(root)))
        .id();
    let mut skin_joints = joint_entities.clone();
    skin_joints.push(extra_unboned);

    for (mesh, material, meta) in &loaded.meshes {
        commands.spawn((
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material.clone()),
            meta.clone(),
            SkinnedMesh {
                inverse_bindposes: loaded.inv_bindposes.clone(),
                joints: skin_joints.clone(),
            },
            NoFrustumCulling,
            Transform::default(),
            ChildOf(root),
        ));
    }

    let mut anim = EnemyAnim::new(loaded.bank.clone(), joint_entities, 0.0);
    anim.play(clip, looping, speed, 0.0);
    commands.entity(root).insert((anim, MapAircraft));
}

fn load_model(
    models: &mut HashMap<String, Option<Vec<Loaded>>>,
    textures: &mut Textures,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    disc: &Disc,
    asset: &str,
    pack: Option<&str>,
) -> Option<Vec<Loaded>> {
    if let Some(cached) = models.get(asset) {
        return cached.clone();
    }
    let loaded = match acvd_render::model(disc, asset) {
        Ok(list) => Some(
            list.into_iter()
                .map(|m| {
                    let material = textures.material(disc, pack, &m, materials, images);
                    let meta = MapMeshMeta {
                        mtd: m.mtd.clone(),
                        diffuse2: m
                            .diffuse2
                            .as_deref()
                            .and_then(|n| textures.image_handle(disc, pack, n, false, images)),
                    };
                    (
                        meshes.add(m.mesh),
                        material,
                        meta,
                        Vec3::from_array(m.min),
                        Vec3::from_array(m.max),
                    )
                })
                .collect(),
        ),
        Err(e) => {
            warn!("{asset}: {e:#}");
            None
        }
    };
    models.insert(asset.to_string(), loaded.clone());
    loaded
}

fn binder_has(
    cache: &mut HashMap<String, HashSet<String>>,
    disc: &Disc,
    binder: &str,
    file: &str,
) -> bool {
    let names = cache.entry(binder.to_string()).or_insert_with(|| {
        vfs::open(disc, binder)
            .ok()
            .and_then(|data| bnd3::read(&data).ok())
            .map(|b| {
                b.entries
                    .iter()
                    .filter_map(|e| e.name.as_deref())
                    .map(|name| {
                        name.rsplit(['\\', '/', ':'])
                            .next()
                            .unwrap_or(name)
                            .to_ascii_lowercase()
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    names.contains(&file.to_ascii_lowercase())
}

fn spawn_lod(
    commands: &mut ChildSpawnerCommands,
    list: &[Loaded],
    visibility: Visibility,
) -> Entity {
    commands
        .spawn((Transform::default(), visibility))
        .with_children(|c| {
            for (mesh, material, meta, _, _) in list {
                c.spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(material.clone()),
                    meta.clone(),
                ));
            }
        })
        .id()
}

/// Shows the full mesh inside the near band and `{model}_l1` past it (`sheets/map_lod.csv`).
/// The classifier's `> 15` return is not a world-distance cull: at the actest camera that
/// reading hid 481 of 550 parts, the small objects, and left the ground under the AC.
pub fn lod(
    camera: Query<&Transform, (With<Camera3d>, Without<crate::env::WaterReflectionCamera>)>,
    parts: Query<(&MapLod, Option<&DestructibleProp>)>,
    mut vis: Query<&mut Visibility>,
    mut logged: Local<bool>,
) {
    let Ok(camera) = camera.single() else { return };
    let eye = camera.translation;
    let (mut n_full, mut n_low) = (0, 0);
    for (part, prop) in &parts {
        if prop.is_some_and(|p| p.broken) {
            continue;
        }
        let metric = lod_metric(eye.distance(part.center), part.radius);
        let full = metric < 0.1 || part.low.is_none();
        set_vis(&mut vis, part.full, full);
        if let Some(low) = part.low {
            set_vis(&mut vis, low, !full);
        }
        if !*logged {
            if full {
                n_full += 1;
            } else {
                n_low += 1;
            }
        }
    }
    if !*logged {
        info!("map lod: {n_full} full, {n_low} _l1");
        *logged = true;
    }
}

/// Handles newly broken destructible map objects (`DestructibleProp::just_broken`): hides their
/// intact meshes, swaps collision `_h.hmd` -> `_b_h.hmd` via `Collision::break_prop`, spawns
/// physics-driven gibs for `_b.flv` models with `break*` / `bk*` bones (falling back to the
/// static `broken_mesh` for single-bone rubble replacements like `o2106`/`o2107`), and plays
/// explosion FFX + destruction SFX (`DestroyModel` `0x82c84a60`).
#[allow(clippy::too_many_arguments)]
pub fn step_destructibles(
    garage: Res<crate::Garage>,
    mut commands: Commands,
    mut collision: ResMut<Collision>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<bevy::mesh::skinning::SkinnedMeshInverseBindposes>>,
    mut props: Query<(&mut DestructibleProp, &Transform)>,
    mut vis: Query<&mut Visibility>,
    mut cues: ResMut<sound::Cues>,
    mut audio_sources: ResMut<Assets<AudioSource>>,
) {
    let mut packs = Packs::default();
    for (mut prop, xf) in &mut props {
        if !prop.just_broken {
            continue;
        }
        prop.just_broken = false;
        collision.break_prop(prop.prop_id);
        set_vis(&mut vis, prop.full, false);
        if let Some(low) = prop.low {
            set_vis(&mut vis, low, false);
        }
        let broken_asset = format!(
            "model/obj/{}/{}_m.bnd.dcx|{}_b.flv",
            prop.model, prop.model, prop.model
        );
        let spawned_debris = crate::debris::spawn(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut bindposes,
            &garage.disc,
            &mut packs,
            garage.flat,
            &broken_asset,
            &prop.model,
            crate::debris::DebrisKind::Prop,
            *xf,
            Vec3::ZERO,
            Vec3::ZERO,
        )
        .is_some();
        set_vis(&mut vis, prop.broken_mesh, !spawned_debris);

        let sfx_row = if prop.max_hp <= 500.0 && prop.damage_id == 0 {
            501
        } else {
            502
        };
        let sfx_id = find(PARAM_SFXSESETPARAM_BIN, sfx_row)
            .map(|r| r.data.sfx_id as i32)
            .filter(|&id| id > 0)
            .unwrap_or(sfx_row as i32);
        commands.spawn((
            sfx::Sfx::new(sfx_id),
            Transform::from_translation(prop.center),
        ));
        if let Some(se) = find(PARAM_ACSOUNDPARAM_BIN, 0)
            .filter(|r| r.data.se_destroy > 0)
            .map(|r| format!("c{:08}", r.data.se_destroy))
        {
            sound::cue_at(
                &mut commands,
                &mut cues,
                &mut audio_sources,
                &se,
                prop.center,
            );
        }
        info!(
            "destructible prop {} (id {}) broken at ({:.1}, {:.1}, {:.1}) (debris={})",
            prop.model, prop.prop_id, prop.center.x, prop.center.y, prop.center.z, spawned_debris
        );
    }
}

fn set_vis(vis: &mut Query<&mut Visibility>, entity: Entity, on: bool) {
    let Ok(mut vis) = vis.get_mut(entity) else {
        return;
    };
    vis.set_if_neq(if on {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    });
}

/// `xf` (FLVER axes) acting on meshes that `acvd-render` already mirrored on X: `S xf S` with
/// `S = diag(-1, 1, 1)`.
pub(crate) fn bevy_transform(xf: &flver::Xform) -> Transform {
    let s = [-1.0f32, 1.0, 1.0];
    let m = |r: usize, c: usize| s[r] * s[c] * xf.m[r][c];
    let mat = Mat4::from_cols(
        Vec4::new(m(0, 0), m(1, 0), m(2, 0), 0.0),
        Vec4::new(m(0, 1), m(1, 1), m(2, 1), 0.0),
        Vec4::new(m(0, 2), m(1, 2), m(2, 2), 0.0),
        Vec4::new(-xf.t[0], xf.t[1], xf.t[2], 1.0),
    );
    Transform::from_matrix(mat)
}

/// FFX ids a static object TAE fires at frame 0, with each event dummy's model-space position
/// and forward. TAE `dummy_id` is FLVER dummy `color[1]`, as on AC parts (`sfx::effect_points`).
/// Object TAEs are the `tae/obj/<oXXXX>/own000100.tae` files on the 360 disc (loader `0x8284e7c8`,
/// `acvd_formats::tae`); they carry the same `KIND_FFX` (0) dispatch as AC TAEs (`0x8289acd0`).
fn object_effects(disc: &Disc, model: &str, asset: &str) -> Vec<(i32, [f32; 3], [f32; 3])> {
    let Ok(bytes) = vfs::open(disc, &format!("tae/obj/{model}/own000100.tae")) else {
        return Vec::new();
    };
    let Ok(tae) = tae::read(&bytes) else {
        return Vec::new();
    };
    let mut fired: Vec<(i32, u8)> = Vec::new();
    for anim in &tae.animations {
        for ev in &anim.events {
            if ev.start_frame != 0 {
                continue;
            }
            if let Some((id, dummy)) = ev.as_ffx() {
                if id > 0 && !fired.contains(&(id, dummy)) {
                    fired.push((id, dummy));
                }
            }
        }
    }
    if fired.is_empty() {
        return Vec::new();
    }
    let Ok(data) = vfs::open(disc, asset) else {
        return Vec::new();
    };
    let Ok(f) = flver::read(&data) else {
        return Vec::new();
    };
    let bones = f.bone_transforms().unwrap_or_default();
    let mut out = Vec::new();
    for (id, dummy_id) in fired {
        let Some(d) = f.dummies.iter().find(|d| d.color[1] == dummy_id) else {
            continue;
        };
        let bt = usize::try_from(d.parent_bone)
            .ok()
            .and_then(|i| bones.get(i));
        let (position, forward) = match bt {
            Some(bt) => {
                let f = [0, 1, 2].map(|r| {
                    bt.m[r][0] * d.forward[0]
                        + bt.m[r][1] * d.forward[1]
                        + bt.m[r][2] * d.forward[2]
                });
                (bt.apply(d.position), f)
            }
            None => (d.position, d.forward),
        };
        out.push((id, position, forward));
    }
    out
}

/// Spawns a persistent map-object FFX on its TAE dummy: the effect sits at the dummy socket with
/// +Z along the dummy forward, mirrored into Bevy axes like the model.
fn spawn_object_effect(
    c: &mut ChildSpawnerCommands,
    ffx_id: i32,
    position: [f32; 3],
    forward: [f32; 3],
) {
    let position = Vec3::new(-position[0], position[1], position[2]);
    let forward = Vec3::new(-forward[0], forward[1], forward[2]).normalize_or(Vec3::Z);
    c.spawn((
        sfx::Sfx::new(ffx_id),
        Transform::from_translation(position)
            .with_rotation(Quat::from_rotation_arc(Vec3::Z, forward)),
        Visibility::default(),
    ));
}

/// Map materials keyed by `(diffuse, bump, alpha_kind)` so opaque and alpha-blended/masked/additive
/// MTDs sharing a texture name never collide.
#[derive(Default)]
struct Textures {
    own: String,
    packs: Packs,
    by_name:
        HashMap<(Option<String>, Option<String>, Option<String>, u8), Handle<StandardMaterial>>,
    images: HashMap<(String, String, bool), Option<Handle<Image>>>,
    missing: usize,
}

/// Classifies a FLVER map MTD's blend / alpha-test mode (`Map_Diffuse_Alpha`, `Map_Speculer_Alpha`,
/// `Map_Diffuse_AlphaMask`, `Map_Diffuse_Add`).
pub(crate) fn mtd_alpha_mode(mtd: &str) -> (AlphaMode, u8) {
    let stem = mtd
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(mtd)
        .trim_end_matches(".mtd")
        .to_ascii_lowercase();
    if stem.contains("alphamask") {
        (AlphaMode::Mask(0.5), 1)
    } else if stem.ends_with("_alpha") || stem.contains("_alpha_") {
        (AlphaMode::Blend, 2)
    } else if stem.ends_with("_add") || stem.contains("_add_") {
        (AlphaMode::Add, 3)
    } else {
        (AlphaMode::Opaque, 0)
    }
}

/// FLVER material texture names the 360 resolves to runtime render targets instead of a TPF on the
/// disc. `FUN_82646580` (`0x82646580`) builds the scene's named-target map at `this+0xa2010` and
/// `FUN_82801168` (`0x82801168`, vtable `0x8208ce30` slot `0x28`) sets/erases its `movie` entry;
/// `FUN_82bd2b00` (`0x82bd2b00`) is the name lookup, `FUN_82bd2c40` (`0x82bd2c40`) the insert.
/// `g_Color`/`g_Decal` sit in the render-target table (`FUN_82bc0ec8` ids 1 and 6) and `movie` is
/// the FMV player's surface (`FUN_824a8890` `0x824a8890`, global fallback `0x83a91474`). None of
/// them exists as a texture in any TPF (`private/index` texture scan: 34,521 refs, no `movie`), so
/// they must not be counted as missing map assets (`o0006` `signboardGrow` names `movie` twice).
const RUNTIME_TEXTURES: [&str; 3] = ["movie", "g_color", "g_decal"];

fn is_runtime_texture(name: &str) -> bool {
    RUNTIME_TEXTURES
        .iter()
        .any(|t| name.eq_ignore_ascii_case(t))
}

impl Textures {
    fn load_tex(
        &mut self,
        disc: &Disc,
        pack: Option<&str>,
        name: &str,
        linear: bool,
    ) -> Result<Image, anyhow::Error> {
        if let Some(tpf) = pack {
            // Object model textures are named entries of its own `{model}.tpf.dcx`.
            if let Some(index) = self.packs.find(disc, tpf, name).ok().flatten() {
                return if linear {
                    self.packs.linear_texture_at(disc, tpf, index, name)
                } else {
                    self.packs.texture_at(disc, tpf, index, name)
                };
            }
        } else {
            let own = format!("{}|{name}.tpf.dcx", self.own);
            if disc.exists(&own) {
                return if linear {
                    self.packs.linear_texture_at(disc, &own, 0, name)
                } else {
                    self.packs.texture_at(disc, &own, 0, name)
                };
            }
        }
        // Shared texture with no per-model/per-map entry: global name lookup.
        if linear {
            self.packs.linear_image(disc, name)
        } else {
            self.packs.image(disc, name)
        }
    }

    fn image_handle(
        &mut self,
        disc: &Disc,
        pack: Option<&str>,
        name: &str,
        linear: bool,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        let key = (pack.unwrap_or("").to_string(), name.to_string(), linear);
        if let Some(cached) = self.images.get(&key) {
            return cached.clone();
        }
        let handle = match self.load_tex(disc, pack, name, linear) {
            Ok(i) => Some(images.add(i)),
            Err(e) => {
                // Runtime render targets are absent in AC TEST by design (no FMV playing), not
                // missing disc assets; the white fallback is the 360's unbound-sampler result.
                if !linear && !is_runtime_texture(name) {
                    self.missing += 1;
                }
                debug!("{e:#}");
                None
            }
        };
        self.images.insert(key, handle.clone());
        handle
    }

    fn material(
        &mut self,
        disc: &Disc,
        pack: Option<&str>,
        mesh: &LoadedMesh,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
    ) -> Handle<StandardMaterial> {
        let (alpha_mode, alpha_kind) = mtd_alpha_mode(&mesh.mtd);
        let key = (
            pack.map(str::to_string),
            mesh.diffuse.clone(),
            mesh.bump.clone(),
            alpha_kind,
        );
        if let Some(h) = self.by_name.get(&key) {
            return h.clone();
        }
        let image = mesh
            .diffuse
            .as_deref()
            .and_then(|name| self.image_handle(disc, pack, name, false, images));
        let normal_map = mesh
            .bump
            .as_deref()
            .and_then(|name| self.image_handle(disc, pack, name, true, images));
        let handle = materials.add(StandardMaterial {
            base_color: if image.is_some() {
                Color::WHITE
            } else {
                Color::srgb(0.55, 0.56, 0.58)
            },
            base_color_texture: image,
            normal_map_texture: normal_map,
            flip_normal_map_y: false,
            perceptual_roughness: 0.9,
            alpha_mode,
            cull_mode: if alpha_kind != 0 {
                None
            } else {
                Some(bevy::render::render_resource::Face::Back)
            },
            ..default()
        });
        self.by_name.insert(key, handle.clone());
        handle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_render_targets_are_not_missing_assets() {
        assert!(is_runtime_texture("movie"));
        assert!(is_runtime_texture("g_Color"));
        assert!(is_runtime_texture("g_decal"));
        assert!(!is_runtime_texture("o0006_01"));
    }

    #[test]
    fn mirrored_transform_matches_mirrored_points() {
        let part = msb::Part {
            name: String::new(),
            model: String::new(),
            kind: 0,
            param_index: 0,
            behavior: None,
            translation: [10.0, 2.0, -3.0],
            rotation_deg: [15.0, 70.0, -20.0],
            scale: [1.0, 2.0, 0.5],
            light_set: 1,
            follow_camera_xz: false,
            is_sky: false,
            layer_index: None,
            layer: None,
            ac_design_id: None,
        };
        let xf = xform(&part);
        let t = bevy_transform(&xf);
        let p = [1.5f32, -0.5, 4.0];
        let game = xf.apply(p);
        let bevy = t.transform_point(Vec3::new(-p[0], p[1], p[2]));
        assert!((bevy - Vec3::new(-game[0], game[1], game[2])).length() < 1e-4);
    }

    #[test]
    fn o0006_sign_tae_spawns_ffx_197_on_its_two_lamp_dummies() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        let fx = object_effects(&disc, "o0006", "model/obj/o0006/o0006_m.bnd.dcx|o0006.flv");
        assert_eq!(
            fx.len(),
            2,
            "o0006 TAE fires ffx 197 on two dummies: {fx:?}"
        );
        for (id, pos, _) in fx {
            assert_eq!(id, 197);
            // Dummies 200/201 are the two lamp arms (±19.67, 18.47, -7.15) in model space.
            assert!((pos[1] - 18.47).abs() < 0.05, "pos {pos:?}");
            assert!((pos[0].abs() - 19.67).abs() < 0.05, "pos {pos:?}");
        }
    }

    #[test]
    fn m3330_aircraft_formations_load_with_spinning_rotors() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        let parts =
            msb::parts(&vfs::open(&disc, "model/map/m3330/m3330_map.msb").unwrap()).unwrap();
        let chinooks: Vec<&msb::Part> = parts.iter().filter(|p| p.model == "o9310").collect();
        let bombers: Vec<&msb::Part> = parts.iter().filter(|p| p.model == "o9330").collect();
        assert_eq!(chinooks.len(), 8, "m3330 o9310 placements");
        assert_eq!(bombers.len(), 1, "m3330 o9330 placement");
        assert_eq!(
            chinooks
                .iter()
                .map(|p| p.behavior.as_deref())
                .collect::<Vec<_>>(),
            vec![
                Some("775"),
                Some("776"),
                Some("777"),
                Some("778"),
                Some("778"),
                Some("778"),
                Some("777"),
                Some("776"),
            ]
        );
        assert_eq!(bombers[0].behavior.as_deref(), Some("770"));

        let (meshes, rig) =
            acvd_render::rigged_model(&disc, "model/obj/o9310/o9310_m.bnd.dcx|o9310.flv", &|_| {
                acvd_render::RootPlace::IDENTITY
            })
            .unwrap();
        assert!(!meshes.is_empty());
        let bank = EnemyAnimBank::load_object(&disc, "o9310", &rig);
        let tae9310 = tae::read(&vfs::open(&disc, "tae/obj/o9310/own000100.tae").unwrap()).unwrap();
        for part in &chinooks {
            let (tae_id, _) = behavior_tae(part).expect("o9310 behavior row");
            let event = tae9310
                .animations
                .iter()
                .find(|a| a.id == tae_id)
                .and_then(|a| {
                    a.events
                        .iter()
                        .find(|e| e.kind == tae::KIND_ANI && e.params.len() >= 20)
                })
                .expect("o9310 kind-104 ANI event");
            let clip =
                u16::try_from(u32::from_be_bytes(event.params[0..4].try_into().unwrap())).unwrap();
            assert!(
                bank.clips.contains_key(&clip),
                "{} behavior {:?} -> clip {clip}",
                part.name,
                part.behavior
            );
        }
        let rotor = rig
            .bones
            .iter()
            .position(|b| b.name.to_ascii_lowercase().starts_with("wing"))
            .expect("o9310 rotor bone (`wing1`..`wing8`)");
        assert!(
            bank.clips.contains_key(&0),
            "o9310 a00_000 is the rotor flight loop"
        );
        let mut anim = EnemyAnim::new(bank, Vec::new(), 0.0);
        anim.play(0, true, 1.0, 0.0);
        let mut prev = anim.sample_bone(rotor).rotation;
        let mut max_delta = 0.0f32;
        for step in 1..=40 {
            anim.frame = step as f32 * 5.0;
            let cur = anim.sample_bone(rotor).rotation;
            max_delta = max_delta.max(prev.angle_between(cur));
            prev = cur;
        }
        assert!(max_delta > 0.01, "o9310 rotor must move across a00_000");

        let (meshes, rig) =
            acvd_render::rigged_model(&disc, "model/obj/o9330/o9330_m.bnd.dcx|o9330.flv", &|_| {
                acvd_render::RootPlace::IDENTITY
            })
            .unwrap();
        assert!(!meshes.is_empty());
        let bank = EnemyAnimBank::load_object(&disc, "o9330", &rig);
        assert!(
            bank.clips.contains_key(&210),
            "o9330 _a binder ships only a00_210"
        );
    }

    #[test]
    fn migrant_transport_behavior_clips_spin_rotors() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };

        let m3100 =
            msb::parts(&vfs::open(&disc, "model/map/m3100/m3100_map.msb").unwrap()).unwrap();
        let tae9340 = tae::read(&vfs::open(&disc, "tae/obj/o9340/own000100.tae").unwrap()).unwrap();
        let transports: Vec<&msb::Part> = m3100.iter().filter(|p| p.model == "o9340").collect();
        assert!(!transports.is_empty(), "m3100 migrant transports");
        for part in &transports {
            assert_eq!(part.behavior.as_deref(), Some("810"));
            let (tae_id, looping) = behavior_tae(part).expect("o9340 behavior row");
            assert_eq!(tae_id, 0x8c0);
            assert!(looping, "behavior 4 loops");
            let event = tae9340
                .animations
                .iter()
                .find(|a| a.id == tae_id)
                .and_then(|a| {
                    a.events
                        .iter()
                        .find(|e| e.kind == tae::KIND_ANI && e.params.len() >= 20)
                })
                .expect("o9340 kind-104 ANI event");
            let clip =
                u16::try_from(u32::from_be_bytes(event.params[0..4].try_into().unwrap())).unwrap();
            assert_eq!(clip, 220);
        }

        let (meshes, rig) =
            acvd_render::rigged_model(&disc, "model/obj/o9340/o9340_m.bnd.dcx|o9340.flv", &|_| {
                acvd_render::RootPlace::IDENTITY
            })
            .unwrap();
        assert!(!meshes.is_empty());
        let bank = EnemyAnimBank::load_object(&disc, "o9340", &rig);
        assert!(bank.clips.contains_key(&220), "o9340 a00_220");
        let rotor = rig
            .bones
            .iter()
            .position(|b| b.name.eq_ignore_ascii_case("l_pro"))
            .expect("o9340 l_pro rotor");
        let mut anim = EnemyAnim::new(bank, Vec::new(), 0.0);
        anim.play(220, true, 1.0, 0.0);
        let mut prev = anim.sample_bone(rotor).rotation;
        let mut max_delta = 0.0f32;
        for step in 1..=40 {
            anim.frame = step as f32 * 5.0;
            let cur = anim.sample_bone(rotor).rotation;
            max_delta = max_delta.max(prev.angle_between(cur));
            prev = cur;
        }
        assert!(max_delta > 0.01, "o9340 l_pro rotor must spin");

        let m3120 =
            msb::parts(&vfs::open(&disc, "model/map/m3120/m3120_map.msb").unwrap()).unwrap();
        let tae9341 = tae::read(&vfs::open(&disc, "tae/obj/o9341/own000100.tae").unwrap()).unwrap();
        let tae9343 = tae::read(&vfs::open(&disc, "tae/obj/o9343/own000100.tae").unwrap()).unwrap();
        for part in m3120.iter().filter(|p| p.model == "o9341") {
            let behavior = part.behavior.as_deref().expect("o9341 behavior row");
            let expected = match behavior {
                "811" => (0x8c1, 221),
                "775" => (0x89b, 210),
                "776" => (0x89f, 211),
                other => panic!("unexpected o9341 behavior {other}"),
            };
            let (tae_id, _) = behavior_tae(part).unwrap();
            assert_eq!(tae_id, expected.0, "o9341 behavior {behavior}");
            let event = tae9341
                .animations
                .iter()
                .find(|a| a.id == tae_id)
                .and_then(|a| a.events.iter().find(|e| e.kind == tae::KIND_ANI))
                .unwrap();
            let clip =
                u16::try_from(u32::from_be_bytes(event.params[0..4].try_into().unwrap())).unwrap();
            assert_eq!(clip, expected.1, "o9341 behavior {behavior}");
        }
        for part in m3120.iter().filter(|p| p.model == "o9343") {
            assert_eq!(part.behavior.as_deref(), Some("812"));
            let (tae_id, _) = behavior_tae(part).unwrap();
            assert_eq!(tae_id, 0x8c2);
            let event = tae9343
                .animations
                .iter()
                .find(|a| a.id == tae_id)
                .and_then(|a| a.events.iter().find(|e| e.kind == tae::KIND_ANI))
                .unwrap();
            let clip =
                u16::try_from(u32::from_be_bytes(event.params[0..4].try_into().unwrap())).unwrap();
            assert_eq!(clip, 222);
        }

        let (meshes, rig) =
            acvd_render::rigged_model(&disc, "model/obj/o9343/o9343_m.bnd.dcx|o9343.flv", &|_| {
                acvd_render::RootPlace::IDENTITY
            })
            .unwrap();
        assert!(!meshes.is_empty());
        let bank = EnemyAnimBank::load_object(&disc, "o9343", &rig);
        assert!(bank.clips.contains_key(&222), "o9343 a00_222");
        let rotor = rig
            .bones
            .iter()
            .position(|b| b.name.eq_ignore_ascii_case("f_rotor"))
            .expect("o9343 f_rotor");
        let mut anim = EnemyAnim::new(bank, Vec::new(), 0.0);
        anim.play(222, true, 1.0, 0.0);
        let mut prev = anim.sample_bone(rotor).rotation;
        let mut max_delta = 0.0f32;
        for step in 1..=40 {
            anim.frame = step as f32 * 5.0;
            let cur = anim.sample_bone(rotor).rotation;
            max_delta = max_delta.max(prev.angle_between(cur));
            prev = cur;
        }
        assert!(max_delta > 0.01, "o9343 f_rotor must spin");
    }

    #[test]
    fn object_textures_resolve_from_their_own_pack() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        let mut part = msb::Part {
            name: String::new(),
            model: "o2076".into(),
            kind: KIND_OBJECT,
            param_index: 0,
            behavior: None,
            translation: [0.0; 3],
            rotation_deg: [0.0; 3],
            scale: [1.0; 3],
            light_set: 0,
            follow_camera_xz: false,
            is_sky: false,
            layer_index: None,
            layer: None,
            ac_design_id: None,
        };
        assert_eq!(
            texture_pack(&disc, &part).as_deref(),
            Some("model/obj/o2076/o2076.tpf.dcx")
        );
        // The fence's `net` is a thin dark lattice inside o2076's own pack; the global
        // name lookup would have picked the dense, brighter m3200 copy instead.
        let mut packs = Packs::default();
        assert_eq!(
            packs
                .find(&disc, "model/obj/o2076/o2076.tpf.dcx", "net")
                .unwrap(),
            Some(1)
        );
        // Map pieces use `{name}.tpf.dcx` members of `{map}_htdcx.bnd`.
        part.kind = KIND_PIECE;
        assert!(texture_pack(&disc, &part).is_none());
    }

    #[test]
    fn lod_bands_match_the_actest_pieces() {
        // m0012 sits on the start camera (full). m0010 is a few hundred metres out (_l1).
        let near = lod_metric(36.0, 144.0);
        let far = lod_metric(242.0, 128.0);
        assert!(near < 0.1, "{near}");
        assert!((0.4..2.0).contains(&far), "{far}");
        assert!(lod_metric(10_000.0, 128.0) > 15.0);
    }

    #[test]
    fn actest_area_bounds_parse_and_classify_zones() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        let bounds = AreaBounds::load(&disc, "m4000", Some("actest")).unwrap();
        let op = bounds.operation.expect("m4000 operation area (kind 100)");
        let warn = bounds.warning.expect("m4000 warning area (kind 101)");
        let caut = bounds.caution.expect("m4000 caution area (kind 102)");
        assert_eq!(op.origin, Vec3::new(804.0, -15.0, 609.35));
        assert_eq!((op.half_x, op.half_z, op.height), (700.0, 450.0, 225.0));
        assert_eq!(
            (warn.half_x, warn.half_z, warn.height),
            (675.0, 425.0, 175.0)
        );
        assert_eq!(
            (caut.half_x, caut.half_z, caut.height),
            (650.0, 400.0, 125.0)
        );

        // AC start point (324.65, 29.0, 649.5) is safely inside all three boxes.
        let s = start(&disc, "m4000", Some("actest")).unwrap().unwrap();
        assert_eq!(bounds.zone(s.position), AreaZone::Inside);

        // Moving past caution (+660 m from center X=804 -> x=1464) triggers Caution (`LIMIT OF AREA`).
        assert_eq!(
            bounds.zone(Vec3::new(1464.0, 30.0, 609.35)),
            AreaZone::Caution
        );
        // Moving past warning (+685 m -> x=1489) triggers Warning (`LIMIT OF AREA`).
        assert_eq!(
            bounds.zone(Vec3::new(1489.0, 30.0, 609.35)),
            AreaZone::Warning
        );
        // Moving past operation (+720 m -> x=1524) is Outside and gets cleanly clamped back in.
        let mut p = Vec3::new(1524.0, 30.0, 609.35);
        let mut v = Vec3::new(2.0, 0.0, 0.5);
        assert_eq!(bounds.enforce(&mut p, &mut v), AreaZone::Outside);
        assert!(p.x <= 804.0 + 700.0 - 0.99, "clamped x = {}", p.x);
        assert_eq!(v.x, 0.0, "outward velocity zeroed");
        assert_eq!(v.z, 0.5, "tangential velocity preserved");
    }

    #[test]
    fn water_return_points_load_from_m4000_and_pick_nearest() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        let wr = WaterReturnPoints::load(&disc, "m4000", Some("actest"), None).unwrap();
        assert_eq!(wr.water_height, Some(3.0));
        assert!(
            !wr.points.is_empty(),
            "m4000 should expose POINT_WATER_RETURN (kind 50) points"
        );
        let fallback = Start {
            position: Vec3::ZERO,
            yaw: 0.0,
        };
        let first = wr.points[0];
        let picked = wr.nearest(first.position + Vec3::new(0.5, -10.0, 0.5), fallback);
        assert!((picked.position - first.position).length() < 1e-3);
    }

    #[test]
    fn destructible_prop_apply_damage_breaks_once() {
        let mut prop = DestructibleProp {
            prop_id: 42,
            model: "o2022".into(),
            center: Vec3::new(10.0, 5.0, 20.0),
            radius: 4.0,
            hp: 500.0,
            max_hp: 500.0,
            cutoff_damage: 0.0,
            collision_damage_mult: 0.0,
            damage_id: 89112,
            full: Entity::PLACEHOLDER,
            low: None,
            broken_mesh: Entity::PLACEHOLDER,
            broken: false,
            just_broken: false,
        };
        assert_eq!(prop.max_hp, 500.0);
        assert_eq!(prop.damage_id, 89112);
        // Fuel tank (collision_defensibility=100 -> mult=0.0) ignores AC body collision.
        assert!(!prop.is_collision_destructible());
        assert!(!prop.apply_collision_damage(9999.0));
        assert!(!prop.apply_damage(200.0));
        assert!(!prop.broken && !prop.just_broken);
        assert!((prop.hp - 300.0).abs() < 1e-4);
        assert!(prop.apply_damage(400.0));
        assert!(prop.broken && prop.just_broken && prop.hp == 0.0);
        // Subsequent hits do not re-trigger break.
        assert!(!prop.apply_damage(500.0));
    }
}
