//! Map look from `ch_env/{map}_env.msb` (`sheets/map_env.csv`): map pieces switch to the
//! `env_lit.wgsl` material with their light set's constants, the AC's Bevy lights take light set
//! 0, and the clear colour comes from the scene record.

use std::collections::HashMap;

use acvd_formats::env::{self, Env, LightSet};
use acvd_formats::vfs::{self, Disc};
use acvd_formats::{bnd3, flver, msb};
use acvd_render::Packs;
use bevy::asset::embedded_asset;
use bevy::camera::Exposure;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use bevy::transform::TransformSystems;

use crate::map::{self, MapPart};

pub type EnvMaterial = ExtendedMaterial<StandardMaterial, EnvLit>;

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct EnvLit {
    #[uniform(100)]
    params: EnvParams,
}

/// The light set's pixel-shader constants (`sheets/map_env.csv` light.* / fog.* rows), in
/// Bevy axes.
#[derive(ShaderType, Debug, Clone, Copy, Default)]
struct EnvParams {
    /// c3 / c4.
    dir0: Vec4,
    dir1: Vec4,
    /// c5 / c6.
    col0: Vec4,
    col1: Vec4,
    /// Hemisphere rows c32..c34 split into `(A + B) / 2` and `(A - B) / 2` per channel.
    amb_mid: Vec4,
    amb_half: Vec4,
    /// c13, c14 (`w`, the camera height, comes from the view).
    fog_dist: Vec4,
    fog_height: Vec4,
    /// c9 / c10.
    fog_col0: Vec4,
    fog_col1: Vec4,
}

impl MaterialExtension for EnvLit {
    fn fragment_shader() -> ShaderRef {
        "embedded://acvd_game/env_lit.wgsl".into()
    }
}

pub type SkyMaterial = ExtendedMaterial<StandardMaterial, SkyLit>;

/// `Map_Sky` objects (`env_sky.wgsl`), drawn behind everything.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct SkyLit {}

impl MaterialExtension for SkyLit {
    fn fragment_shader() -> ShaderRef {
        "embedded://acvd_game/env_sky.wgsl".into()
    }

    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }
}

/// The diffuse texture of object `o`'s FLVER when its material is `Map_Sky`, from the object's
/// own `model/obj/{o}/{o}.tpf.dcx` (the map loader only searches the map's texture binder).
fn sky_texture(disc: &Disc, packs: &mut Packs, o: &str) -> Option<Image> {
    let binder = format!("model/obj/{o}/{o}_m.bnd.dcx");
    let data = vfs::open(disc, &binder).ok()?;
    let b = bnd3::read(&data).ok()?;
    let flv = format!("{o}.flv").to_ascii_lowercase();
    let entry = b.entries.iter().find(|e| {
        e.name
            .as_deref()
            .is_some_and(|n| n.to_ascii_lowercase().ends_with(&flv))
    })?;
    let f = flver::read(&vfs::undcx(entry.contents(&data).ok()?.into_owned()).ok()?).ok()?;
    if !f.materials.iter().any(|m| m.mtd.contains("Map_Sky")) {
        return None;
    }
    let name = acvd_render::model(disc, &format!("{binder}|{o}.flv"))
        .ok()?
        .into_iter()
        .find_map(|m| m.diffuse)?;
    let pack = format!("model/obj/{o}/{o}.tpf.dcx");
    let index = packs.find(disc, &pack, &name).ok()??;
    packs
        .texture_at(disc, &pack, index, &name)
        .map_err(|e| warn!("{pack}: {e:#}"))
        .ok()
}

/// Light-set slots the 360 picked per draw in the AC test (`sheets/map_env.csv` draw.sets).
const SET_AC: u32 = 0;
const SET_MAP: u32 = 1;
const SET_MOUNTAIN: u32 = 2;
const SET_WATER: u32 = 3;

/// The light set a map model draws with. The 360 reads the id from a field not yet found; this
/// reproduces the ids it chose on m4000: the `Map_Diffuse_Multi` mountains m9000-m9049 use 2,
/// the `Water` models m91xx use 3, everything else 1.
fn model_slot(model: &str) -> u32 {
    match model.strip_prefix('m').and_then(|n| n.parse::<u32>().ok()) {
        Some(9000..=9049) => SET_MOUNTAIN,
        Some(9100..=9199) => SET_WATER,
        _ => SET_MAP,
    }
}

/// `rgb^2 * w`, the 360 colour conversion with the gamma flag set (0x82c2bc48, 0x82bd2268).
fn conv(c: [f32; 4]) -> Vec3 {
    Vec3::new(c[0] * c[0], c[1] * c[1], c[2] * c[2]) * c[3]
}

fn squared(c: [f32; 4]) -> Vec3 {
    Vec3::new(c[0] * c[0], c[1] * c[1], c[2] * c[2])
}

/// Game axes to Bevy: the map meshes are mirrored on X.
fn bevy_dir(d: [f32; 3]) -> Vec3 {
    Vec3::new(-d[0], d[1], d[2])
}

fn params(set: &LightSet) -> EnvParams {
    let sky = conv(set.sky);
    let ground = conv(set.ground);
    let f = &set.fog;
    let falloff = f.falloff.max(0.1);
    let w = if f.rising { 1.0 / falloff } else { -1.0 / falloff };
    let inv = |v: f32| if v > 0.0 { 1.0 / v } else { 0.0 };
    EnvParams {
        dir0: bevy_dir(set.direction(0)).extend(0.0),
        dir1: bevy_dir(set.direction(1)).extend(0.0),
        col0: conv(set.colors[0]).extend(0.0),
        col1: conv(set.colors[1]).extend(0.0),
        amb_mid: ((sky + ground) * 0.5).extend(0.0),
        amb_half: ((sky - ground) * 0.5).extend(0.0),
        fog_dist: Vec4::new(f.color[3] * f.density, f.start, inv(f.range), w),
        fog_height: Vec4::new(f.height_color[3] * f.density, f.height_base, inv(f.height_range) * w, 0.0),
        fog_col0: squared(f.color).extend(f.luma_tint),
        fog_col1: squared(f.height_color).extend(f.luma_tint),
    }
}

pub struct EnvPlugin {
    pub disc: Disc,
}

impl Plugin for EnvPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "env_lit.wgsl");
        embedded_asset!(app, "env_sky.wgsl");
        app.add_plugins((
            MaterialPlugin::<EnvMaterial>::default(),
            MaterialPlugin::<SkyMaterial>::default(),
        ))
            .insert_resource(EnvDisc(self.disc.clone()))
            .add_systems(PostStartup, load)
            .add_systems(Update, relight)
            .add_systems(PostUpdate, follow_sky.before(TransformSystems::Propagate));
    }
}

/// A `Map_Sky` part: the 360 draws it at the camera's X / Z, its own height, scaled by the scene
/// record's sky scale (probe `private/xenia/sky_obj.txt`: object matrix diag(100) at
/// (cam x, 0, cam z) in m4000's sky pass).
#[derive(Component)]
struct SkyDome {
    y: f32,
    scale: f32,
}

fn follow_sky(
    camera: Query<&Transform, With<Camera3d>>,
    mut domes: Query<(&SkyDome, &mut Transform), Without<Camera3d>>,
) {
    let Ok(camera) = camera.single() else { return };
    for (dome, mut xf) in &mut domes {
        xf.translation = Vec3::new(camera.translation.x, dome.y, camera.translation.z);
        xf.scale = Vec3::splat(dome.scale);
    }
}

#[derive(Resource)]
struct EnvDisc(Disc);

/// The loaded env, the terrain parts' models by part name, the env materials made so far, and
/// each object model's sky material (`None`: not `Map_Sky`).
#[derive(Resource)]
struct MapEnv {
    env: Env,
    models: HashMap<String, String>,
    made: HashMap<(AssetId<StandardMaterial>, u32), Handle<EnvMaterial>>,
    skies: HashMap<String, Option<Handle<SkyMaterial>>>,
    packs: Packs,
}

fn env_path(map: &str) -> String {
    format!("model/map/ch_env/{map}_env.msb")
}

/// Bevy illuminance whose Lambert term (`albedo / pi * E * exposure`) equals the 360's
/// `albedo * colour` at the default camera exposure.
fn lux(linear: f32) -> f32 {
    linear * std::f32::consts::PI / Exposure::default().exposure()
}

fn load(
    mut commands: Commands,
    disc: Res<EnvDisc>,
    scene: Res<crate::Scene>,
    mut lights: Query<(&mut DirectionalLight, &mut Transform)>,
    cameras: Query<Entity, With<Camera3d>>,
) {
    let Some(map) = scene.map.as_deref() else { return };
    let disc = &disc.0;
    let path = env_path(map);
    let env = match vfs::open(disc, &path).and_then(|d| env::read(&d)) {
        Ok(env) => env,
        Err(e) => {
            warn!("{path}: {e:#}");
            return;
        }
    };
    let models = vfs::open(disc, &map::terrain_msb(disc, map))
        .and_then(|d| msb::parts(&d))
        .map(|parts| parts.into_iter().map(|p| (p.name, p.model)).collect())
        .unwrap_or_default();
    if let Some(s) = env.scene {
        commands.insert_resource(ClearColor(Color::srgb_u8(s.clear[0], s.clear[1], s.clear[2])));
    }
    if let Some(ac) = env.light(SET_AC) {
        for (i, (mut light, mut xf)) in lights.iter_mut().enumerate().take(2) {
            let c = conv(ac.colors[i]);
            let peak = c.max_element().max(1e-6);
            light.color = Color::linear_rgb(c.x / peak, c.y / peak, c.z / peak);
            light.illuminance = lux(peak);
            *xf = Transform::IDENTITY.looking_to(-bevy_dir(ac.direction(i)), Vec3::Y);
        }
        let amb = (conv(ac.sky) + conv(ac.ground)) * 0.5;
        let peak = amb.max_element().max(1e-6);
        commands.insert_resource(GlobalAmbientLight {
            color: Color::linear_rgb(amb.x / peak, amb.y / peak, amb.z / peak),
            brightness: peak / Exposure::default().exposure(),
            ..default()
        });
    }
    for camera in &cameras {
        commands.entity(camera).insert(Tonemapping::None);
    }
    info!("{path}: {} light sets, scene {:?}", env.lights.len(), env.scene);
    commands.insert_resource(MapEnv {
        env,
        models,
        made: HashMap::new(),
        skies: HashMap::new(),
        packs: Packs::default(),
    });
}

/// Moves every newly spawned map-part mesh onto the env material of its model's light set, or
/// onto the sky material when the part is a `Map_Sky` object.
#[allow(clippy::too_many_arguments)]
fn relight(
    mut commands: Commands,
    disc: Res<EnvDisc>,
    env: Option<ResMut<MapEnv>>,
    added: Query<(Entity, &MeshMaterial3d<StandardMaterial>), Added<MeshMaterial3d<StandardMaterial>>>,
    parents: Query<&ChildOf>,
    parts: Query<(&Name, &Transform), With<MapPart>>,
    standard: Res<Assets<StandardMaterial>>,
    mut materials: ResMut<Assets<EnvMaterial>>,
    mut sky_materials: ResMut<Assets<SkyMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(mut env) = env else { return };
    let env = &mut *env;
    for (entity, material) in &added {
        let Some((part, (name, part_xf))) =
            parents.iter_ancestors(entity).find_map(|a| parts.get(a).ok().map(|p| (a, p)))
        else {
            continue;
        };
        let model = env.models.get(name.as_str()).cloned().unwrap_or_default();
        if model.starts_with('o') {
            let sky = env
                .skies
                .entry(model.clone())
                .or_insert_with(|| {
                    let image = sky_texture(&disc.0, &mut env.packs, &model)?;
                    info!("{model}: Map_Sky dome");
                    Some(sky_materials.add(SkyMaterial {
                        base: StandardMaterial {
                            base_color_texture: Some(images.add(image)),
                            unlit: true,
                            cull_mode: None,
                            ..default()
                        },
                        extension: SkyLit {},
                    }))
                })
                .clone();
            if let Some(sky) = sky {
                commands
                    .entity(entity)
                    .remove::<MeshMaterial3d<StandardMaterial>>()
                    .insert(MeshMaterial3d(sky));
                let scale = env.env.scene.map_or(1.0, |s| s.sky_scale);
                commands.entity(part).insert(SkyDome { y: part_xf.translation.y, scale });
                continue;
            }
        }
        let slot = model_slot(&model);
        let Some(set) = env.env.light(slot).or_else(|| env.env.light(SET_MAP)) else {
            continue;
        };
        let Some(base) = standard.get(&material.0) else { continue };
        let handle = env
            .made
            .entry((material.0.id(), slot))
            .or_insert_with(|| {
                materials.add(EnvMaterial {
                    base: base.clone(),
                    extension: EnvLit { params: params(set) },
                })
            })
            .clone();
        commands
            .entity(entity)
            .remove::<MeshMaterial3d<StandardMaterial>>()
            .insert(MeshMaterial3d(handle));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountains_and_water_take_their_sets() {
        assert_eq!(model_slot("m9002"), SET_MOUNTAIN);
        assert_eq!(model_slot("m9101"), SET_WATER);
        assert_eq!(model_slot("m0012"), SET_MAP);
        assert_eq!(model_slot("o2146"), SET_MAP);
    }
}
