//! Pure map simulation data: start points, water-return points, area zones, and destructible
//! object state. Bevy spawning, meshes, debris, and FFX stay in `acvd-game`.

use acvd_data::find;
use acvd_data::generated::ac_unit::PARAM_ACOBJECTPARAM_BIN;
use acvd_data::generated::enemy::PARAM_DESTROYAP_PARAM;
use acvd_formats::vfs::{self, Disc};
use acvd_formats::{env as fmt_env, flver, mlb, msb};
use anyhow::Result;
use glam::{Vec2, Vec3};

use crate::ids::PropId;

/// Map part kinds that have a visual model and a hit model.
pub const KIND_PIECE: u32 = 0;
pub const KIND_OBJECT: u32 = 1;

/// Map asset folder.
pub fn folder(map: &str) -> String {
    format!("model/map/{map}")
}

/// Full disc asset path for `file` of `part`: a VD piece binder entry, an ACV loose piece
/// file, or an object binder entry. `None` for kinds without map geometry.
pub fn model_asset(disc: &Disc, map: &str, part: &msb::Part, file: &str) -> Option<String> {
    match part.kind {
        KIND_PIECE => {
            let binder = format!("{}/{map}_m.dcx.bnd", folder(map));
            if disc.exists(&binder) {
                Some(format!("{binder}|{file}"))
            } else {
                Some(format!("{}/{file}", folder(map)))
            }
        }
        KIND_OBJECT => Some(format!("model/obj/{0}/{0}_m.bnd.dcx|{file}", part.model)),
        _ => None,
    }
}

/// The part's placement in FLVER (game) axes.
pub fn xform(part: &msb::Part) -> flver::Xform {
    flver::Xform::local(
        part.translation,
        part.rotation_deg.map(f32::to_radians),
        part.scale,
    )
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

/// Where the player AC starts: world position and yaw (radians, as `AcMotion::yaw`).
#[derive(Clone, Copy, Debug)]
pub struct Start {
    pub position: Vec3,
    pub yaw: f32,
}

/// Water-return respawn points (`POINT_PARAM_ST` kind `50`, `msb::POINT_WATER_RETURN`) and
/// water surface height (`env::Scene.water_height`).
#[derive(Clone, Debug, Default)]
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

    /// Water-return point closest to `pos` in XZ, or `fallback` if none exist.
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

/// Simulation state of a destructible map object. The Bevy entity that draws it and hides the
/// meshes keeps this as its `sim` field.
#[derive(Clone, Debug)]
pub struct Destructible {
    /// Matches `collision::Hit::prop_id` (`part_idx + 1`).
    pub prop_id: PropId,
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
    pub broken: bool,
    /// Set on the lethal hit; consumed by the presentation layer.
    pub just_broken: bool,
}

impl Destructible {
    /// Whether this prop can be broken by AC body collision (`0x829aabe8`: `mult >= 0.001`).
    pub fn is_collision_destructible(&self) -> bool {
        !self.broken && self.collision_damage_mult >= 0.001 && self.cutoff_damage < 65535.0
    }

    /// Applies weapon/explosion `damage` (`0x829ad0f8`); returns `true` on the hit that breaks it.
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
    /// scaling `raw_damage` by `collision_damage_mult` (`0x82c01bd8`).
    pub fn apply_collision_damage(&mut self, raw_damage: f32) -> bool {
        if !self.is_collision_destructible() || raw_damage <= 0.0 {
            return false;
        }
        self.apply_damage(raw_damage * self.collision_damage_mult)
    }
}

/// All destructible props of one map, with their break state.
#[derive(Clone, Debug, Default)]
pub struct DestructibleSet {
    props: Vec<Destructible>,
}

impl DestructibleSet {
    pub fn new(mut props: Vec<Destructible>) -> Self {
        props.sort_by_key(|p| p.prop_id.0);
        Self { props }
    }

    pub fn props(&self) -> &[Destructible] {
        &self.props
    }

    pub fn props_mut(&mut self) -> &mut [Destructible] {
        &mut self.props
    }

    pub fn get(&self, id: PropId) -> Option<&Destructible> {
        self.props.iter().find(|p| p.prop_id == id)
    }

    pub fn get_mut(&mut self, id: PropId) -> Option<&mut Destructible> {
        self.props.iter_mut().find(|p| p.prop_id == id)
    }

    /// Applies weapon/explosion damage. Returns `true` when this hit broke the prop.
    pub fn apply_damage(&mut self, id: PropId, damage: f32) -> bool {
        self.get_mut(id).is_some_and(|p| p.apply_damage(damage))
    }

    /// Applies AC body-collision damage (`0x82865470`). Returns `true` when this hit broke it.
    pub fn apply_collision_damage(&mut self, id: PropId, raw_damage: f32) -> bool {
        self.get_mut(id)
            .is_some_and(|p| p.apply_collision_damage(raw_damage))
    }

    /// Marks a prop broken from an authoritative reliable delta.
    pub fn break_prop(&mut self, id: PropId) -> bool {
        self.get_mut(id).is_some_and(|p| {
            let was = p.broken;
            p.hp = 0.0;
            p.broken = true;
            p.just_broken |= !was;
            !was
        })
    }

    /// Broken bits, one per prop in `prop_id` order, for late-join bulk state.
    pub fn broken_bits(&self) -> Vec<u8> {
        let mut bits = vec![0u8; self.props.len().div_ceil(8)];
        for (i, p) in self.props.iter().enumerate() {
            if p.broken {
                bits[i / 8] |= 1 << (i % 8);
            }
        }
        bits
    }
}

/// Durability/collision metadata read from `{model}_m.mlb` and `PARAM_DESTROYAP_PARAM`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DestructibleMeta {
    pub damage_id: i32,
    pub max_hp: f32,
    pub cutoff_damage: f32,
    pub collision_damage_mult: f32,
}

/// Full disc asset path of `{model}_m.mlb` for a map object or piece.
fn destructible_mlb_path(disc: &Disc, map: &str, model: &str, is_object: bool) -> String {
    if is_object {
        format!("model/obj/{model}/{model}_m.bnd.dcx|{model}_m.mlb")
    } else {
        let binder = format!("{}/{map}_m.dcx.bnd", folder(map));
        if disc.exists(&binder) {
            format!("{binder}|{model}_m.mlb")
        } else {
            format!("{}/{model}_m.mlb", folder(map))
        }
    }
}

/// Reads the destructible metadata for `model` (`0x82c01de8` / `0x82c01bd8`).
pub fn load_destructible_meta(
    disc: &Disc,
    map: &str,
    model: &str,
    is_object: bool,
) -> DestructibleMeta {
    let obj_id = model
        .strip_prefix('o')
        .or_else(|| model.strip_prefix('O'))
        .and_then(|s| s.parse::<u32>().ok());
    let damage_id = obj_id
        .and_then(|id| find(PARAM_ACOBJECTPARAM_BIN, id))
        .map_or(0, |r| r.data.damage_id);
    let destroy_ap_id = {
        let path = destructible_mlb_path(disc, map, model, is_object);
        vfs::open(disc, &path)
            .ok()
            .and_then(|b| mlb::read_destroy_ap_ids(&b).ok())
            .and_then(|ids| ids.primary_id())
    };
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
    DestructibleMeta {
        damage_id,
        max_hp,
        cutoff_damage,
        collision_damage_mult,
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

/// Map AABB used by the snapshot position quantizer.
pub fn map_aabb(disc: &Disc, map: &str) -> Option<(Vec3, Vec3)> {
    crate::collision::load_map(disc, map).ok()?.bounds()
}

/// Builds the destructible simulation set for `map`.
///
/// Mirrors the game's map spawn: every `KIND_OBJECT` part whose binder also carries
/// `{model}_b.flv` is a destructible. Bounds come from that prop's collision triangles so the
/// server and every client agree on center/radius without loading meshes.
pub fn load_destructibles(disc: &Disc, map: &str) -> Result<DestructibleSet> {
    let parts = msb::parts(&vfs::open(disc, &terrain_msb(disc, map))?)?;
    let collision = crate::collision::load_map(disc, map).ok();
    let mut props = Vec::new();
    for (part_idx, part) in parts.iter().enumerate() {
        if part.kind != KIND_OBJECT {
            continue;
        }
        let broken = model_asset(disc, map, part, &format!("{}_b.flv", part.model));
        if !broken.is_some_and(|asset| vfs::open(disc, &asset).is_ok()) {
            continue;
        }
        let prop_id = (part_idx + 1) as u32;
        let (center, radius) = collision
            .as_ref()
            .and_then(|c| prop_bounds(c, prop_id))
            .unwrap_or_else(|| {
                (
                    Vec3::new(
                        -part.translation[0],
                        part.translation[1],
                        part.translation[2],
                    ),
                    2.0,
                )
            });
        let meta = load_destructible_meta(disc, map, &part.model, true);
        props.push(Destructible {
            prop_id: PropId(prop_id),
            model: part.model.clone(),
            center,
            radius: radius.max(2.0),
            hp: meta.max_hp,
            max_hp: meta.max_hp,
            cutoff_damage: meta.cutoff_damage,
            collision_damage_mult: meta.collision_damage_mult,
            damage_id: meta.damage_id,
            broken: false,
            just_broken: false,
        });
    }
    Ok(DestructibleSet::new(props))
}

fn prop_bounds(collision: &crate::collision::Collision, prop_id: u32) -> Option<(Vec3, f32)> {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    let mut any = false;
    for hit in collision.hits.iter().filter(|h| h.prop_id == prop_id) {
        any = true;
        min = min.min(hit.a).min(hit.b).min(hit.c);
        max = max.max(hit.a).max(hit.b).max(hit.c);
    }
    any.then(|| {
        let center = (min + max) * 0.5;
        (center, (max - min).length() * 0.5)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prop(hp: f32) -> Destructible {
        Destructible {
            prop_id: PropId(1),
            model: "o1".into(),
            center: Vec3::ZERO,
            radius: 2.0,
            hp,
            max_hp: hp,
            cutoff_damage: 10.0,
            collision_damage_mult: 1.0,
            damage_id: 0,
            broken: false,
            just_broken: false,
        }
    }

    #[test]
    fn destructible_breaks_once_and_ignores_cutoff() {
        let mut p = prop(100.0);
        assert!(!p.apply_damage(10.0), "damage at the cutoff is ignored");
        assert!(!p.apply_damage(50.0));
        assert_eq!(p.hp, 50.0);
        assert!(p.apply_damage(60.0));
        assert!(p.broken && p.just_broken);
        assert!(!p.apply_damage(1000.0), "already broken");
    }

    #[test]
    fn destructible_set_tracks_broken_bits() {
        let mut a = prop(10.0);
        a.prop_id = PropId(1);
        let mut b = prop(10.0);
        b.prop_id = PropId(2);
        let mut set = DestructibleSet::new(vec![b, a]);
        assert!(set.apply_damage(PropId(2), 11.0));
        assert_eq!(set.broken_bits(), vec![0b10]);
        assert!(!set.apply_damage(PropId(1), 5.0));
    }
}
