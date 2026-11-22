//! Map look from `ch_env/{map}_env.msb` (`sheets/map_env.csv`): map pieces switch to the
//! `env_lit.wgsl` material (supporting single-texture `Flver_ColDif` and two-texture
//! `Map_Diffuse_Multi` / `Flver_ColDifMul`) with their light set's constants, water pieces
//! (`Water.spx`) switch to `env_water.wgsl` (`Flver_Water`), the AC's Bevy lights take light set
//! 0, the clear colour comes from the scene record, and HDR `Bloom` is configured from type-101
//! sub-block 2 (`m_ToneMapFilter`, `0x82c18628`).

use std::collections::HashMap;

use acvd_formats::env::{self, Env, LightSet};
use acvd_formats::vfs::{self, Disc};
use acvd_formats::{bnd3, flver, msb};
use acvd_render::Packs;
use bevy::asset::{embedded_asset, RenderAssetUsages};
use bevy::camera::{Exposure, Hdr};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::post_process::bloom::{Bloom, BloomCompositeMode, BloomPrefilter};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat,
};
use bevy::shader::ShaderRef;
use bevy::transform::TransformSystems;

use crate::map::{self, MapMeshMeta, MapPart};

pub type EnvMaterial = ExtendedMaterial<StandardMaterial, EnvLit>;

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct EnvLit {
    #[uniform(100)]
    params: EnvParams,
    #[texture(101)]
    #[sampler(102)]
    diffuse1: Handle<Image>,
    #[texture(103)]
    #[sampler(104)]
    diffuse2: Handle<Image>,
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
    /// `m_ToneMapFilter` (`0x82c18628`): `(c32 = 1 / (lum * key), c33 = mul, c34 = white, mode)`.
    tonemap: Vec4,
    /// `x = has_diffuse1`, `y = has_diffuse2` (`Map_Diffuse_Multi`).
    flags: Vec4,
}

impl MaterialExtension for EnvLit {
    fn fragment_shader() -> ShaderRef {
        "embedded://acvd_game/env_lit.wgsl".into()
    }
}

pub type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterLit>;

/// `Water.spx` surface (`env_water.wgsl`, `Normal/Flver_Water.fpo`, `m4000_mtd.bnd` `Water.mtd`).
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct WaterLit {
    #[uniform(100)]
    params: WaterParams,
    #[texture(101)]
    #[sampler(102)]
    bump: Handle<Image>,
}

#[derive(ShaderType, Debug, Clone, Copy, Default)]
struct WaterParams {
    dir0: Vec4,
    dir1: Vec4,
    /// c11 / c12 (`colors_b`), directional water specular colours.
    spec0: Vec4,
    spec1: Vec4,
    amb_mid: Vec4,
    amb_half: Vec4,
    fog_dist: Vec4,
    fog_height: Vec4,
    fog_col0: Vec4,
    fog_col1: Vec4,
    tonemap: Vec4,
    /// `g_WaterColor` (`m4000_mtd.bnd` `Water.mtd`: `(0.15, 0.10, 0.05, 1.0)`).
    water_color: Vec4,
    /// `(g_FresnelPow, g_FresnelBias, g_FresnelScale, g_SpecularPower)`.
    fresnel: Vec4,
    /// `(g_TileScale_0, g_TileScale_1, g_TileScale_2, g_BumpMapSmoose)`.
    tile_scale: Vec4,
    /// `(g_TexScroll_0.xy, g_TexScroll_1.xy)`.
    scroll01: Vec4,
    /// `(g_TexScroll_2.xy, elapsed_secs, has_bump)`.
    scroll2_time: Vec4,
}

impl MaterialExtension for WaterLit {
    fn fragment_shader() -> ShaderRef {
        "embedded://acvd_game/env_water.wgsl".into()
    }
}

pub type SkyMaterial = ExtendedMaterial<StandardMaterial, SkyLit>;

/// `Map_Sky` objects (`env_sky.wgsl`), drawn behind everything.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct SkyLit {
    #[uniform(100)]
    tonemap: Vec4,
    #[texture(101)]
    #[sampler(102)]
    sky_tex: Handle<Image>,
}

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

/// Default light-set slots (`sheets/map_env.csv` draw.sets).
const SET_AC: u32 = 0;
const SET_MAP: u32 = 1;

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

/// `m_ToneMapFilter` pixel-shader constants (`0x82c18628`): `(c32, c33, c34, mode)`.
fn tonemap_vec(tm: env::ToneMap) -> Vec4 {
    let denom = (tm.lum * tm.key).max(1e-6);
    Vec4::new(1.0 / denom, tm.mul, tm.white, tm.mode as f32)
}

/// Maps type-101 sub-block 2 bloom fields (`0x82c18580`, `0x82c17908`, `0x82c18308`) onto Bevy's
/// HDR `Bloom` component (`None` when `+0x1a` `bloom_enable` is 0).
pub(crate) fn bevy_bloom(b: env::Bloom) -> Option<Bloom> {
    if !b.enabled {
        return None;
    }
    let scale = b.color[3].clamp(0.0, 2.0);
    Some(Bloom {
        intensity: (0.22 * scale).clamp(0.0, 1.0),
        low_frequency_boost: 0.5,
        low_frequency_boost_curvature: 0.75,
        high_pass_frequency: 1.0,
        prefilter: BloomPrefilter {
            threshold: b.threshold.max(0.0),
            threshold_softness: (b.offset * 0.5).clamp(0.0, 1.0),
        },
        composite_mode: BloomCompositeMode::Additive,
        ..default()
    })
}

/// True for FLVER materials backed by `Flver_Water*` (`Water.spx`, `WaterRiver`, `WaterWave`),
/// excluding `Water_Mask` (which uses `Flver_ColDif`).
pub(crate) fn is_water_mtd(mtd: &str) -> bool {
    let stem = mtd
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(mtd)
        .trim_end_matches(".mtd")
        .to_ascii_lowercase();
    matches!(stem.as_str(), "water" | "waterriver" | "waterwave")
}

fn params(set: &LightSet, tonemap: Vec4, has_diffuse1: bool, has_diffuse2: bool) -> EnvParams {
    let sky = conv(set.sky);
    let ground = conv(set.ground);
    let f = &set.fog;
    let falloff = f.falloff.max(0.1);
    let w = if f.rising {
        1.0 / falloff
    } else {
        -1.0 / falloff
    };
    let inv = |v: f32| if v > 0.0 { 1.0 / v } else { 0.0 };
    EnvParams {
        dir0: bevy_dir(set.direction(0)).extend(0.0),
        dir1: bevy_dir(set.direction(1)).extend(0.0),
        col0: conv(set.colors[0]).extend(0.0),
        col1: conv(set.colors[1]).extend(0.0),
        amb_mid: ((sky + ground) * 0.5).extend(0.0),
        amb_half: ((sky - ground) * 0.5).extend(0.0),
        fog_dist: Vec4::new(f.color[3] * f.density, f.start, inv(f.range), w),
        fog_height: Vec4::new(
            f.height_color[3] * f.density,
            f.height_base,
            inv(f.height_range) * w,
            0.0,
        ),
        fog_col0: squared(f.color).extend(f.luma_tint),
        fog_col1: squared(f.height_color).extend(f.luma_tint),
        tonemap,
        flags: Vec4::new(
            if has_diffuse1 { 1.0 } else { 0.0 },
            if has_diffuse2 { 1.0 } else { 0.0 },
            0.0,
            0.0,
        ),
    }
}

fn water_params(set: &LightSet, tonemap: Vec4, has_bump: bool) -> WaterParams {
    let p = params(set, tonemap, false, false);
    WaterParams {
        dir0: p.dir0,
        dir1: p.dir1,
        spec0: conv(set.colors_b[0]).extend(0.0),
        spec1: conv(set.colors_b[1]).extend(0.0),
        amb_mid: p.amb_mid,
        amb_half: p.amb_half,
        fog_dist: p.fog_dist,
        fog_height: p.fog_height,
        fog_col0: p.fog_col0,
        fog_col1: p.fog_col1,
        tonemap,
        // `material/m4000_mtd.bnd` `Water.mtd` (`sheets/map_env.csv` `ps.water` row).
        water_color: Vec4::new(0.15, 0.10, 0.05, 1.0),
        fresnel: Vec4::new(70.0, 0.5, 0.9, 10.0),
        tile_scale: Vec4::new(0.0019, 0.00268, 0.0462, 2.0),
        scroll01: Vec4::new(0.01, 0.03, -0.01, -0.02),
        scroll2_time: Vec4::new(-0.05, 0.05, 0.0, if has_bump { 1.0 } else { 0.0 }),
    }
}

fn solid_pixel(rgba: [u8; 4], format: TextureFormat) -> Image {
    Image::new_fill(
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &rgba,
        format,
        RenderAssetUsages::default(),
    )
}

pub struct EnvPlugin {
    pub disc: Disc,
}

impl Plugin for EnvPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "env_lit.wgsl");
        embedded_asset!(app, "env_sky.wgsl");
        embedded_asset!(app, "env_water.wgsl");
        app.add_plugins((
            MaterialPlugin::<EnvMaterial>::default(),
            MaterialPlugin::<SkyMaterial>::default(),
            MaterialPlugin::<WaterMaterial>::default(),
        ))
        .insert_resource(EnvDisc(self.disc.clone()))
        .add_systems(PostStartup, load)
        .add_systems(Update, (relight, tick_water))
        .add_systems(PostUpdate, follow_sky.before(TransformSystems::Propagate));
    }
}

/// A `Map_Sky` part whose `MODEL_PARAM_ST` `+0x14` sub-record `+1` bit 7 is set: the 360
/// overwrites world-matrix X/Z with camera X/Z (`0x829911f0` / `0x8299f5e4`; probe
/// `private/xenia/sky_obj.txt`: object matrix diag(100) at (cam x, 0, cam z) on m4000).
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

fn tick_water(time: Res<Time>, mut waters: ResMut<Assets<WaterMaterial>>) {
    let t = time.elapsed_secs();
    for (_, mat) in waters.iter_mut() {
        mat.extension.params.scroll2_time.z = t;
    }
}

#[derive(Resource)]
struct EnvDisc(Disc);

/// The loaded env, the terrain parts by part name, the env/water materials made so far, and
/// each object model's sky material (`None`: not `Map_Sky`).
#[derive(Resource)]
struct MapEnv {
    env: Env,
    parts_by_name: HashMap<String, msb::Part>,
    made: HashMap<(AssetId<StandardMaterial>, u32, Option<AssetId<Image>>), Handle<EnvMaterial>>,
    waters: HashMap<(Option<AssetId<Image>>, u32), Handle<WaterMaterial>>,
    skies: HashMap<String, Option<Handle<SkyMaterial>>>,
    packs: Packs,
    white_srgb: Handle<Image>,
    flat_normal: Handle<Image>,
}

fn env_path(map: &str) -> String {
    format!("model/map/ch_env/{map}_env.msb")
}

/// Bevy illuminance whose Lambert term (`albedo / pi * E * exposure`) equals the 360's
/// `albedo * colour` at the default camera exposure.
fn lux(linear: f32) -> f32 {
    linear * std::f32::consts::PI / Exposure::default().exposure()
}

/// Applies an env MSB's clear color, AC light set (`SET_AC`), global ambient light, and camera
/// HDR / bloom settings. Returns the parsed [`Env`] when the file loads.
pub(crate) fn apply_env_file(
    commands: &mut Commands,
    disc: &Disc,
    path: &str,
    lights: &mut Query<(&mut DirectionalLight, &mut Transform)>,
    cameras: &Query<Entity, With<Camera3d>>,
) -> Option<Env> {
    let env = match vfs::open(disc, path).and_then(|d| env::read(&d)) {
        Ok(env) => env,
        Err(e) => {
            warn!("{path}: {e:#}");
            return None;
        }
    };
    if let Some(s) = env.scene.as_ref() {
        commands.insert_resource(ClearColor(Color::srgb_u8(
            s.clear[0], s.clear[1], s.clear[2],
        )));
        crate::lens_flare::set_active(commands, s.lens_flare.clone());
    } else {
        crate::lens_flare::clear_active(commands);
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
    let bloom = env
        .scene
        .as_ref()
        .map(|s| s.tonemap.bloom)
        .and_then(bevy_bloom);
    for camera in cameras {
        let mut cam = commands.entity(camera);
        cam.insert((Hdr, Tonemapping::None));
        if let Some(bloom) = bloom.clone() {
            cam.insert(bloom);
        } else {
            cam.remove::<Bloom>();
        }
    }
    info!(
        "{path}: {} light sets, scene {:?}",
        env.lights.len(),
        env.scene
    );
    Some(env)
}

/// Loads `ch_env/{map}_env.msb`, applies its AC lights & bloom, and installs [`MapEnv`] so map
/// parts are relit by [`relight`].
pub(crate) fn load_map_env(
    commands: &mut Commands,
    disc: &Disc,
    map: &str,
    lights: &mut Query<(&mut DirectionalLight, &mut Transform)>,
    cameras: &Query<Entity, With<Camera3d>>,
    images: &mut Assets<Image>,
) {
    let path = env_path(map);
    let Some(env) = apply_env_file(commands, disc, &path, lights, cameras) else {
        return;
    };
    let parts_by_name = vfs::open(disc, &map::terrain_msb(disc, map))
        .and_then(|d| msb::parts(&d))
        .map(|parts| parts.into_iter().map(|p| (p.name.clone(), p)).collect())
        .unwrap_or_default();
    let white_srgb = images.add(solid_pixel([255, 255, 255, 255], TextureFormat::Rgba8UnormSrgb));
    let flat_normal = images.add(solid_pixel([128, 128, 255, 255], TextureFormat::Rgba8Unorm));
    commands.insert_resource(MapEnv {
        env,
        parts_by_name,
        made: HashMap::new(),
        waters: HashMap::new(),
        skies: HashMap::new(),
        packs: Packs::default(),
        white_srgb,
        flat_normal,
    });
}

/// Removes active sortie map-env relighting state (e.g. when returning to the 3D garage).
pub(crate) fn clear_map_env(commands: &mut Commands) {
    commands.remove_resource::<MapEnv>();
    crate::lens_flare::clear_active(commands);
}

fn load(
    mut commands: Commands,
    disc: Res<EnvDisc>,
    scene: Res<crate::Scene>,
    mode: Res<crate::garage::Mode>,
    mut lights: Query<(&mut DirectionalLight, &mut Transform)>,
    cameras: Query<Entity, With<Camera3d>>,
    mut images: ResMut<Assets<Image>>,
) {
    if *mode != crate::garage::Mode::Sortie {
        return;
    }
    let Some(map) = scene.map.as_deref() else {
        return;
    };
    load_map_env(&mut commands, &disc.0, map, &mut lights, &cameras, &mut images);
}

/// Moves every newly spawned map-part mesh onto `SkyMaterial` (`Map_Sky`), `WaterMaterial`
/// (`Water.spx`), or `EnvMaterial` (`Flver_ColDif` / `Map_Diffuse_Multi`).
#[allow(clippy::too_many_arguments)]
fn relight(
    mut commands: Commands,
    disc: Res<EnvDisc>,
    env: Option<ResMut<MapEnv>>,
    added: Query<
        (Entity, &MeshMaterial3d<StandardMaterial>, Option<&MapMeshMeta>),
        Added<MeshMaterial3d<StandardMaterial>>,
    >,
    parents: Query<&ChildOf>,
    parts: Query<(&Name, &Transform), With<MapPart>>,
    standard: Res<Assets<StandardMaterial>>,
    mut materials: ResMut<Assets<EnvMaterial>>,
    mut water_materials: ResMut<Assets<WaterMaterial>>,
    mut sky_materials: ResMut<Assets<SkyMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(mut env) = env else { return };
    let env = &mut *env;
    for (entity, material, meta) in &added {
        let Some((part, (name, part_xf))) = parents
            .iter_ancestors(entity)
            .find_map(|a| parts.get(a).ok().map(|p| (a, p)))
        else {
            continue;
        };
        let msb_part = env.parts_by_name.get(name.as_str()).cloned();
        let model = msb_part
            .as_ref()
            .map(|p| p.model.clone())
            .unwrap_or_default();
        let tm = tonemap_vec(env.env.scene.as_ref().map(|s| s.tonemap).unwrap_or_default());
        if model.starts_with('o') {
            let sky = env
                .skies
                .entry(model.clone())
                .or_insert_with(|| {
                    let image = sky_texture(&disc.0, &mut env.packs, &model)?;
                    let handle = images.add(image);
                    info!("{model}: Map_Sky dome");
                    Some(sky_materials.add(SkyMaterial {
                        base: StandardMaterial {
                            base_color_texture: Some(handle.clone()),
                            unlit: true,
                            cull_mode: None,
                            ..default()
                        },
                        extension: SkyLit {
                            tonemap: tm,
                            sky_tex: handle,
                        },
                    }))
                })
                .clone();
            if let Some(sky) = sky {
                commands
                    .entity(entity)
                    .remove::<MeshMaterial3d<StandardMaterial>>()
                    .insert(MeshMaterial3d(sky));
                if msb_part.as_ref().is_some_and(|p| p.follow_camera_xz) {
                    let scale = env.env.scene.as_ref().map_or(1.0, |s| s.sky_scale);
                    commands.entity(part).insert(SkyDome {
                        y: part_xf.translation.y,
                        scale,
                    });
                }
                continue;
            }
        }
        let slot = msb_part.as_ref().map_or(SET_MAP, |p| p.light_set as u32);
        let Some(set) = env.env.light(slot).or_else(|| env.env.light(SET_MAP)) else {
            continue;
        };
        let Some(base) = standard.get(&material.0) else {
            continue;
        };
        if meta.is_some_and(|m| is_water_mtd(&m.mtd)) {
            let bump_id = base.normal_map_texture.as_ref().map(Handle::id);
            let handle = env
                .waters
                .entry((bump_id, slot))
                .or_insert_with(|| {
                    let has_bump = base.normal_map_texture.is_some();
                    let bump = base
                        .normal_map_texture
                        .clone()
                        .unwrap_or_else(|| env.flat_normal.clone());
                    water_materials.add(WaterMaterial {
                        base: StandardMaterial {
                            unlit: true,
                            ..default()
                        },
                        extension: WaterLit {
                            params: water_params(set, tm, has_bump),
                            bump,
                        },
                    })
                })
                .clone();
            commands
                .entity(entity)
                .remove::<MeshMaterial3d<StandardMaterial>>()
                .insert(MeshMaterial3d(handle));
            continue;
        }
        let diffuse2_handle = meta.and_then(|m| m.diffuse2.clone());
        let diffuse2_id = diffuse2_handle.as_ref().map(Handle::id);
        let handle = env
            .made
            .entry((material.0.id(), slot, diffuse2_id))
            .or_insert_with(|| {
                let has_d1 = base.base_color_texture.is_some();
                let has_d2 = diffuse2_handle.is_some();
                let diffuse1 = base
                    .base_color_texture
                    .clone()
                    .unwrap_or_else(|| env.white_srgb.clone());
                let diffuse2 = diffuse2_handle
                    .clone()
                    .unwrap_or_else(|| env.white_srgb.clone());
                materials.add(EnvMaterial {
                    base: base.clone(),
                    extension: EnvLit {
                        params: params(set, tm, has_d1, has_d2),
                        diffuse1,
                        diffuse2,
                    },
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
    fn tonemap_constants_match_m4000() {
        let v = tonemap_vec(env::ToneMap::default());
        assert!((v.x - 1.0 / 0.3).abs() < 1e-5);
        assert!((v.y - 1.1).abs() < 1e-5);
        assert!((v.z - 10.0).abs() < 1e-5);
        assert_eq!(v.w, 1.0);
    }

    #[test]
    fn bloom_settings_from_m4000_env() {
        let b = bevy_bloom(env::Bloom::default()).expect("m4000 bloom enabled");
        assert!((b.prefilter.threshold - 0.9).abs() < 1e-5);
        assert!((b.prefilter.threshold_softness - 0.4).abs() < 1e-5);
        assert!((b.intensity - 0.22).abs() < 1e-5);
        assert_eq!(b.composite_mode, BloomCompositeMode::Additive);

        let off = env::Bloom {
            enabled: false,
            ..Default::default()
        };
        assert!(bevy_bloom(off).is_none());
    }

    #[test]
    fn water_mtd_detection() {
        assert!(is_water_mtd("Water"));
        assert!(is_water_mtd("Water.mtd"));
        assert!(is_water_mtd("model\\material\\Water.mtd"));
        assert!(is_water_mtd("WaterRiver"));
        assert!(is_water_mtd("WaterWave"));
        assert!(!is_water_mtd("Water_Mask"));
        assert!(!is_water_mtd("Map_Diffuse"));
        assert!(!is_water_mtd("Map_Diffuse_Multi"));
    }

    #[test]
    fn water_params_use_light_set_specular_and_fog() {
        let set = LightSet {
            name: "ID03".into(),
            slot: 3,
            angles: [[42, 140], [-6, 30]],
            colors: [[1.0; 4]; 2],
            colors_b: [[1.2, 1.1, 1.0, 2.0], [0.5, 0.5, 0.5, 1.0]],
            sky: [0.5, 0.6, 0.7, 1.0],
            ground: [0.2, 0.2, 0.2, 1.0],
            extra: [0.0; 4],
            fog: env::Fog {
                color: [0.8, 0.8, 0.8, 0.6],
                range: 375.0,
                start: 30.0,
                falloff: 10.0,
                density: 1.0,
                luma_tint: 0.5,
                rising: true,
                height_range: 0.0,
                height_base: 0.0,
                height_color: [0.0; 4],
            },
        };
        let wp = water_params(&set, tonemap_vec(env::ToneMap::default()), true);
        assert!((wp.spec0.x - 1.2 * 1.2 * 2.0).abs() < 1e-4);
        assert_eq!(wp.fresnel, Vec4::new(70.0, 0.5, 0.9, 10.0));
        assert_eq!(wp.scroll2_time.w, 1.0);
        let ep = params(&set, tonemap_vec(env::ToneMap::default()), true, true);
        assert_eq!(ep.flags, Vec4::new(1.0, 1.0, 0.0, 0.0));
    }
}
