//! Map scene: the visual FLVERs of a map's MSB parts, and the start point of its layout.
//!
//! A map folder `model/map/{map}` holds `{map}_map.msb` (terrain and objects), optional layout
//! MSBs such as `{map}_actest.msb` (the garage AC test: start points, areas, test enemies, no
//! terrain), `{map}_m.dcx.bnd` (map piece `m####.flv` / `_h.hmd`) and `{map}_htdcx.bnd` (one
//! `{texture}.tpf.dcx` per texture; `{map}_l.tpf.dcx` is the low-resolution copy). Objects
//! (part kind 1) load from `model/obj/{o}/{o}_m.bnd.dcx`.

use std::collections::HashMap;

use acvd_formats::vfs::{self, Disc};
use acvd_formats::{flver, msb};
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
    flver::Xform::local(part.translation, part.rotation_deg.map(f32::to_radians), part.scale)
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
        if let Some(p) = points.iter().find(|p| p.kind == msb::POINT_START && p.kind_index == 0) {
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

/// Spawns every map piece and object of `map`'s terrain MSB. Returns how many parts drew.
pub fn spawn(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    disc: &Disc,
    map: &str,
) -> Result<usize> {
    let parts = msb::parts(&vfs::open(disc, &terrain_msb(disc, map))?)?;
    let mut models: HashMap<String, Option<Vec<(Handle<Mesh>, Handle<StandardMaterial>)>>> = HashMap::new();
    let mut textures = Textures {
        own: format!("{}/{map}_htdcx.bnd", folder(map)),
        ..default()
    };
    let mut drawn = 0;
    for part in &parts {
        let Some(binder) = binder(map, part) else { continue };
        let asset = format!("{binder}|{}.flv", part.model);
        let loaded = models.entry(asset.clone()).or_insert_with(|| match acvd_render::model(disc, &asset) {
            Ok(list) => Some(
                list.into_iter()
                    .map(|m| {
                        let material = textures.material(disc, &m, materials, images);
                        (meshes.add(m.mesh), material)
                    })
                    .collect(),
            ),
            Err(e) => {
                warn!("{asset}: {e:#}");
                None
            }
        });
        let Some(list) = loaded else { continue };
        drawn += 1;
        commands
            .spawn((MapPart, bevy_transform(&xform(part)), Visibility::default(), Name::new(part.name.clone())))
            .with_children(|c| {
                for (mesh, material) in list {
                    c.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(material.clone())));
                }
            });
    }
    if textures.missing > 0 {
        warn!("map {map}: {} textures missing", textures.missing);
    }
    Ok(drawn)
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
    fn material(&mut self, disc: &Disc, mesh: &LoadedMesh, materials: &mut Assets<StandardMaterial>, images: &mut Assets<Image>) -> Handle<StandardMaterial> {
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
            base_color: if image.is_some() { Color::WHITE } else { Color::srgb(0.55, 0.56, 0.58) },
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
}
