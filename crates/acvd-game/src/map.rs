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
fn bevy_transform(xf: &flver::Xform) -> Transform {
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
}
