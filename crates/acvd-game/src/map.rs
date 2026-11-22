//! Map scene: the visual FLVERs of a map's MSB parts, and the start point of its layout.
//!
//! A map folder `model/map/{map}` holds `{map}_map.msb` (terrain and objects), optional layout
//! MSBs such as `{map}_actest.msb` (the garage AC test: start points, areas, test enemies, no
//! terrain), `{map}_m.dcx.bnd` (map piece `m####.flv` / `_h.hmd`) and `{map}_htdcx.bnd` (one
//! `{texture}.tpf.dcx` per texture; `{map}_l.tpf.dcx` is the low-resolution copy). Objects
//! (part kind 1) load from `model/obj/{o}/{o}_m.bnd.dcx`.

use std::collections::{HashMap, HashSet};

use acvd_formats::vfs::{self, Disc};
use acvd_formats::{bnd3, flver, msb};
use acvd_render::{LoadedMesh, Packs};
use anyhow::Result;
use bevy::prelude::*;

/// Map part kinds that have a visual model and a hit model.
const KIND_PIECE: u32 = 0;
const KIND_OBJECT: u32 = 1;

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
        let clamped = self
            .operation
            .is_some_and(|op| op.clamp(pos, vel, 1.0));
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

fn mesh_bounds(list: &[(Handle<Mesh>, Handle<StandardMaterial>, Vec3, Vec3)]) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for (_, _, a, b) in list {
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
    disc: &Disc,
    map: &str,
) -> Result<usize> {
    let parts = msb::parts(&vfs::open(disc, &terrain_msb(disc, map))?)?;
    let mut models: HashMap<String, Option<Vec<Loaded>>> = HashMap::new();
    let mut members: HashMap<String, HashSet<String>> = HashMap::new();
    let mut textures = Textures {
        own: format!("{}/{map}_htdcx.bnd", folder(map)),
        ..default()
    };
    let mut drawn = 0;
    let mut low_parts = 0;
    for part in &parts {
        let Some(binder) = binder(map, part) else {
            continue;
        };
        let asset = format!("{binder}|{}.flv", part.model);
        let Some(list) = load_model(
            &mut models,
            &mut textures,
            meshes,
            materials,
            images,
            disc,
            &asset,
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
        let parent = commands
            .spawn((
                MapPart,
                xf,
                Visibility::default(),
                Name::new(part.name.clone()),
            ))
            .id();
        commands.entity(parent).with_children(|c| {
            full = spawn_lod(c, &list, Visibility::Inherited);
            if let Some(low) = &low {
                low_entity = Some(spawn_lod(c, low, Visibility::Hidden));
            }
        });
        commands.entity(parent).insert(MapLod {
            center,
            radius,
            full,
            low: low_entity,
        });
    }
    if textures.missing > 0 {
        warn!("map {map}: {} textures missing", textures.missing);
    }
    info!("map {map}: {drawn} parts, {low_parts} with an _l1 mesh");
    Ok(drawn)
}

type Loaded = (Handle<Mesh>, Handle<StandardMaterial>, Vec3, Vec3);

fn load_model(
    models: &mut HashMap<String, Option<Vec<Loaded>>>,
    textures: &mut Textures,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    disc: &Disc,
    asset: &str,
) -> Option<Vec<Loaded>> {
    if let Some(cached) = models.get(asset) {
        return cached.clone();
    }
    let loaded = match acvd_render::model(disc, asset) {
        Ok(list) => Some(
            list.into_iter()
                .map(|m| {
                    let material = textures.material(disc, &m, materials, images);
                    (
                        meshes.add(m.mesh),
                        material,
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
            for (mesh, material, _, _) in list {
                c.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(material.clone())));
            }
        })
        .id()
}

/// Shows the full mesh inside the near band and `{model}_l1` past it (`sheets/map_lod.csv`).
/// The classifier's `> 15` return is not a world-distance cull: at the actest camera that
/// reading hid 481 of 550 parts, the small objects, and left the ground under the AC.
pub fn lod(
    camera: Query<&Transform, With<Camera3d>>,
    parts: Query<&MapLod>,
    mut vis: Query<&mut Visibility>,
    mut logged: Local<bool>,
) {
    let Ok(camera) = camera.single() else { return };
    let eye = camera.translation;
    let (mut n_full, mut n_low) = (0, 0);
    for part in &parts {
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

/// Map materials keyed by diffuse texture name: the map's own full-resolution texture first,
/// then the first disc pack with that name.
#[derive(Default)]
struct Textures {
    own: String,
    packs: Packs,
    by_name: HashMap<Option<String>, Handle<StandardMaterial>>,
    missing: usize,
}

impl Textures {
    fn material(
        &mut self,
        disc: &Disc,
        mesh: &LoadedMesh,
        materials: &mut Assets<StandardMaterial>,
        images: &mut Assets<Image>,
    ) -> Handle<StandardMaterial> {
        if let Some(h) = self.by_name.get(&mesh.diffuse) {
            return h.clone();
        }
        let image = mesh.diffuse.as_deref().and_then(|name| {
            let own = format!("{}|{name}.tpf.dcx", self.own);
            let img = if disc.exists(&own) {
                self.packs.texture_at(disc, &own, 0, name)
            } else {
                self.packs.image(disc, name)
            };
            match img {
                Ok(i) => Some(images.add(i)),
                Err(e) => {
                    self.missing += 1;
                    debug!("{e:#}");
                    None
                }
            }
        });
        let handle = materials.add(StandardMaterial {
            base_color: if image.is_some() {
                Color::WHITE
            } else {
                Color::srgb(0.55, 0.56, 0.58)
            },
            base_color_texture: image,
            perceptual_roughness: 0.9,
            ..default()
        });
        self.by_name.insert(mesh.diffuse.clone(), handle.clone());
        handle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrored_transform_matches_mirrored_points() {
        let part = msb::Part {
            name: String::new(),
            model: String::new(),
            kind: 0,
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
        assert_eq!((warn.half_x, warn.half_z, warn.height), (675.0, 425.0, 175.0));
        assert_eq!((caut.half_x, caut.half_z, caut.height), (650.0, 400.0, 125.0));

        // AC start point (324.65, 29.0, 649.5) is safely inside all three boxes.
        let s = start(&disc, "m4000", Some("actest")).unwrap().unwrap();
        assert_eq!(bounds.zone(s.position), AreaZone::Inside);

        // Moving past caution (+660 m from center X=804 -> x=1464) triggers Caution (`LIMIT OF AREA`).
        assert_eq!(bounds.zone(Vec3::new(1464.0, 30.0, 609.35)), AreaZone::Caution);
        // Moving past warning (+685 m -> x=1489) triggers Warning (`LIMIT OF AREA`).
        assert_eq!(bounds.zone(Vec3::new(1489.0, 30.0, 609.35)), AreaZone::Warning);
        // Moving past operation (+720 m -> x=1524) is Outside and gets cleanly clamped back in.
        let mut p = Vec3::new(1524.0, 30.0, 609.35);
        let mut v = Vec3::new(2.0, 0.0, 0.5);
        assert_eq!(bounds.enforce(&mut p, &mut v), AreaZone::Outside);
        assert!(p.x <= 804.0 + 700.0 - 0.99, "clamped x = {}", p.x);
        assert_eq!(v.x, 0.0, "outward velocity zeroed");
        assert_eq!(v.z, 0.5, "tangential velocity preserved");
    }
}
