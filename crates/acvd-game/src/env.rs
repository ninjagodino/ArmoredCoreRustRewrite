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
use bevy::camera::primitives::MeshAabb;
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{Exposure, Hdr, RenderTarget};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::post_process::bloom::{Bloom, BloomCompositeMode, BloomPrefilter};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat,
};
use bevy::shader::ShaderRef;
use bevy::transform::TransformSystems;
use bevy::window::PrimaryWindow;

use crate::map::{self, MapMeshMeta, MapPart};

/// Render layer reserved for `Water.spx` surface meshes so the mirrored planar reflection camera
/// (layer 0 only) never renders the water plane into itself.
pub const WATER_RENDER_LAYER: usize = 1;

/// Marker on the mirrored planar reflection camera (`sheets/map_env.csv` `fog.c14` / `ps.water`)
/// so gameplay/HUD/SFX/garage `.single()` `Camera3d` queries ignore it.
#[derive(Component)]
pub struct WaterReflectionCamera;

const WATER_REFL_DEFAULT_W: u32 = 640;
const WATER_REFL_DEFAULT_H: u32 = 360;

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
    /// `x = has_diffuse1`, `y = has_diffuse2` (`Map_Diffuse_Multi`),
    /// `z = dynamic_lights` (`LightAcc` clustered point + spot lights),
    /// `w = emissive_boost` (`Map_Diffuse _Grow` / `Ene_Diffuse_Glow`).
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
    #[texture(103)]
    #[sampler(104)]
    refl: Handle<Image>,
}

#[derive(ShaderType, Debug, Clone, Copy, Default)]
struct WaterParams {
    dir0: Vec4,
    dir1: Vec4,
    /// c11 / c12 (`colors_b`), directional water specular colours.
    spec0: Vec4,
    spec1: Vec4,
    /// `g_FresnelColor.rgb` (`c132`, `0.4784`) and `w = g_ReflectBand` (`c124`, `0.2`).
    fresnel_color: Vec4,
    /// Fallback linear colour when no planar reflection target is active (`w = has_refl`).
    fallback_refl: Vec4,
    fog_dist: Vec4,
    fog_height: Vec4,
    fog_col0: Vec4,
    fog_col1: Vec4,
    /// `g_WaterColor` (`m4000_mtd.bnd` `Water.mtd`: `(0.15, 0.10, 0.05)` squared for 360 linear).
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

/// `Map_Sky` / `Map_SkyMulti` / `Map_NoLightNoFog` (`env_sky.wgsl`), unlit squared-texel linear HDR.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct SkyLit {
    /// `x > 0.5`: force far-plane depth (`0.0`, reverse Z); `y > 0.5`: preserve texture alpha for
    /// cloud/additive blending; `z`: linear HDR scale (default `1.0`).
    #[uniform(100)]
    sky_flags: Vec4,
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
pub(crate) const SET_AC: u32 = 0;
pub(crate) const SET_MAP: u32 = 1;

/// `rgb^2 * w`, the 360 colour conversion with the gamma flag set (0x82c2bc48, 0x82bd2268).
pub(crate) fn conv(c: [f32; 4]) -> Vec3 {
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
///
/// The 360 writes `col * 0.5` (`c31 = 0.5`) into its HDR target before bloom extraction, whereas
/// our shaders write raw linear HDR `col` and halve inside fullscreen `m_ToneMapFilter`
/// (`zoom_blur.wgsl`), so the threshold is scaled by `2.0` to match the 360 bright-pass cutoff.
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
            threshold: (b.threshold * 2.0).max(0.0),
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

fn params(
    set: &LightSet,
    has_diffuse1: bool,
    has_diffuse2: bool,
    dynamic_lights: bool,
    emissive_boost: f32,
) -> EnvParams {
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
        flags: Vec4::new(
            if has_diffuse1 { 1.0 } else { 0.0 },
            if has_diffuse2 { 1.0 } else { 0.0 },
            if dynamic_lights { 1.0 } else { 0.0 },
            emissive_boost,
        ),
    }
}

/// Builds an [`EnvMaterial`] lit by `set` (e.g. `SET_MAP` for garage workshop meshes) with
/// optional clustered dynamic `PointLight`/`SpotLight` reception (`LightAcc`) and emissive glow.
pub(crate) fn make_env_material(
    base: StandardMaterial,
    set: &LightSet,
    white_srgb: &Handle<Image>,
    dynamic_lights: bool,
    emissive_boost: f32,
) -> EnvMaterial {
    let has_d1 = base.base_color_texture.is_some();
    let diffuse1 = base
        .base_color_texture
        .clone()
        .unwrap_or_else(|| white_srgb.clone());
    EnvMaterial {
        base,
        extension: EnvLit {
            params: params(set, has_d1, false, dynamic_lights, emissive_boost),
            diffuse1,
            diffuse2: white_srgb.clone(),
        },
    }
}

/// Builds a [`SkyMaterial`] (`env_sky.wgsl`, squared-texel linear HDR unlit sky/cloud dome).
pub(crate) fn make_sky_material(
    sky_tex: Handle<Image>,
    alpha_mode: AlphaMode,
    far_plane: bool,
    use_tex_alpha: bool,
    hdr_scale: f32,
) -> SkyMaterial {
    SkyMaterial {
        base: StandardMaterial {
            base_color_texture: Some(sky_tex.clone()),
            unlit: true,
            cull_mode: None,
            alpha_mode,
            ..default()
        },
        extension: SkyLit {
            sky_flags: Vec4::new(
                if far_plane { 1.0 } else { 0.0 },
                if use_tex_alpha { 1.0 } else { 0.0 },
                hdr_scale,
                0.0,
            ),
            sky_tex,
        },
    }
}

pub(crate) fn white_srgb_pixel(images: &mut Assets<Image>) -> Handle<Image> {
    images.add(solid_pixel([255, 255, 255, 255], TextureFormat::Rgba8UnormSrgb))
}

fn water_params(set: &LightSet, has_bump: bool, has_refl: bool, clear: [u8; 3]) -> WaterParams {
    let p = params(set, false, false, false, 0.0);
    // `material/m4000_mtd.bnd` `Water.mtd` (`sheets/map_env.csv` `ps.water` row):
    // `g_WaterColor = (0.15, 0.10, 0.05)` squared for 360 linear space (`0x82bd2268`),
    // `g_FresnelColor = (122/255)^3 = 0.47843`, `g_ReflectBand = 0.2`.
    let clear_lin = Vec3::new(
        (clear[0] as f32 / 255.0).powi(2),
        (clear[1] as f32 / 255.0).powi(2),
        (clear[2] as f32 / 255.0).powi(2),
    );
    WaterParams {
        dir0: p.dir0,
        dir1: p.dir1,
        spec0: conv(set.colors_b[0]).extend(0.0),
        spec1: conv(set.colors_b[1]).extend(0.0),
        fresnel_color: Vec4::new(0.4784314, 0.4784314, 0.4784314, 0.2),
        fallback_refl: clear_lin.extend(if has_refl { 1.0 } else { 0.0 }),
        fog_dist: p.fog_dist,
        fog_height: p.fog_height,
        fog_col0: p.fog_col0,
        fog_col1: p.fog_col1,
        water_color: Vec4::new(0.15 * 0.15, 0.10 * 0.10, 0.05 * 0.05, 1.0),
        fresnel: Vec4::new(70.0, 0.5, 0.9, 10.0),
        tile_scale: Vec4::new(0.0019, 0.00268, 0.0462, 2.0),
        scroll01: Vec4::new(0.01, 0.03, -0.01, -0.02),
        scroll2_time: Vec4::new(-0.05, 0.05, 0.0, if has_bump { 1.0 } else { 0.0 }),
    }
}

fn make_reflection_target(width: u32, height: u32) -> Image {
    let mut img = Image::new_target_texture(width, height, TextureFormat::Rgba16Float, None);
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    img
}

/// Mirrors a camera transform across horizontal plane `y = water_y` using a proper right-handed
/// rotation (`det = +1`, flipping the vertical axis so the reflection target maps directly to
/// `(u, 1 - v)` without inverting backface winding).
fn mirror_camera_pose(main: &Transform, water_y: f32) -> Transform {
    let p = main.translation;
    let fwd = main.rotation * Vec3::NEG_Z;
    let right = main.rotation * Vec3::X;
    let up = main.rotation * Vec3::Y;

    let p_refl = Vec3::new(p.x, 2.0 * water_y - p.y, p.z);
    let fwd_refl = Vec3::new(fwd.x, -fwd.y, fwd.z);
    let right_refl = Vec3::new(right.x, -right.y, right.z);
    let up_refl = Vec3::new(-up.x, up.y, -up.z);
    let rot = Quat::from_mat3(&Mat3::from_cols(right_refl, up_refl, -fwd_refl)).normalize();
    Transform::from_translation(p_refl).with_rotation(rot)
}

/// View-space oblique `near_clip_plane` for Bevy's `PerspectiveProjection` that keeps geometry
/// above `y = water_y - bias` and clips underwater geometry from the mirrored reflection camera.
fn oblique_water_clip_plane(refl_xf: &Transform, water_y: f32) -> Vec4 {
    let q_world = Vec3::new(0.0, water_y - 0.15, 0.0);
    let inv_rot = refl_xf.rotation.inverse();
    let n_view = (inv_rot * Vec3::Y).normalize();
    let q_view = inv_rot * (q_world - refl_xf.translation);
    n_view.extend(-n_view.dot(q_view))
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
        .add_systems(
            PostUpdate,
            (follow_sky, sync_water_reflection).before(TransformSystems::Propagate),
        );
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
    camera: Query<&Transform, (With<Camera3d>, Without<WaterReflectionCamera>)>,
    mut domes: Query<(&SkyDome, &mut Transform), Without<Camera3d>>,
) {
    let Ok(camera) = camera.single() else { return };
    for (dome, mut xf) in &mut domes {
        xf.translation = Vec3::new(camera.translation.x, dome.y, camera.translation.z);
        xf.scale = Vec3::splat(dome.scale);
    }
}

/// Keeps the mirrored planar water-reflection camera synchronized with the main camera (and
/// despawns it when leaving sortie / removing `MapEnv`).
#[allow(clippy::type_complexity)]
fn sync_water_reflection(
    mut commands: Commands,
    env: Option<Res<MapEnv>>,
    window: Query<&Window, With<PrimaryWindow>>,
    main_cam: Query<(&Transform, &Projection), (With<Camera3d>, Without<WaterReflectionCamera>)>,
    mut refl_cam: Query<
        (Entity, &mut Transform, &mut Projection, &mut Camera),
        With<WaterReflectionCamera>,
    >,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(env) = env else {
        for (e, _, _, _) in &refl_cam {
            commands.entity(e).despawn();
        }
        return;
    };
    let Ok((main_xf, main_proj)) = main_cam.single() else {
        return;
    };
    let Ok((_, mut refl_xf, mut refl_proj, mut cam)) = refl_cam.single_mut() else {
        return;
    };

    // Match half-res aspect to the primary window so screen-space reflection UVs align 1:1.
    if let Ok(win) = window.single() {
        let want_w = (win.physical_width() / 2).clamp(256, 1280);
        let want_h = (win.physical_height() / 2).clamp(144, 720);
        if let Some(mut img) = images.get_mut(&env.refl_tex) {
            if img.texture_descriptor.size.width != want_w
                || img.texture_descriptor.size.height != want_h
            {
                img.resize(Extent3d {
                    width: want_w,
                    height: want_h,
                    depth_or_array_layers: 1,
                });
            }
        }
    }

    let water_y = env.water_y;
    let above_water = main_xf.translation.y > water_y + 0.25;
    cam.is_active = above_water;
    if !above_water {
        return;
    }

    let mirrored = mirror_camera_pose(main_xf, water_y);
    *refl_xf = mirrored;

    if let (Projection::Perspective(mp), Projection::Perspective(rp)) =
        (main_proj, refl_proj.as_mut())
    {
        rp.fov = mp.fov;
        rp.near = mp.near;
        rp.far = mp.far;
        rp.near_clip_plane = oblique_water_clip_plane(&mirrored, water_y);
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
    refl_tex: Handle<Image>,
    water_y: f32,
    water_y_from_mesh: bool,
    refl_camera_spawned: bool,
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
    cameras: &Query<Entity, (With<Camera3d>, Without<WaterReflectionCamera>)>,
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
    let tm = env
        .scene
        .as_ref()
        .map(|s| s.tonemap)
        .unwrap_or_default();
    commands.insert_resource(crate::blur::ActiveToneMap(tonemap_vec(tm)));

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
        "{path}: {} light sets, {} point / {} spot lights, scene {:?}",
        env.lights.len(),
        env.point_lights.iter().filter(|l| l.enabled).count(),
        env.spot_lights.iter().filter(|l| l.enabled).count(),
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
    cameras: &Query<Entity, (With<Camera3d>, Without<WaterReflectionCamera>)>,
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
    let refl_tex = images.add(make_reflection_target(
        WATER_REFL_DEFAULT_W,
        WATER_REFL_DEFAULT_H,
    ));
    let water_y = env.scene.as_ref().map_or(0.0, |s| s.water_height);
    commands.insert_resource(MapEnv {
        env,
        parts_by_name,
        made: HashMap::new(),
        waters: HashMap::new(),
        skies: HashMap::new(),
        packs: Packs::default(),
        white_srgb,
        flat_normal,
        refl_tex,
        water_y,
        water_y_from_mesh: false,
        refl_camera_spawned: false,
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
    cameras: Query<Entity, (With<Camera3d>, Without<WaterReflectionCamera>)>,
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
        (
            Entity,
            &MeshMaterial3d<StandardMaterial>,
            Option<&Mesh3d>,
            Option<&MapMeshMeta>,
        ),
        Added<MeshMaterial3d<StandardMaterial>>,
    >,
    parents: Query<&ChildOf>,
    parts: Query<(&Name, &Transform), With<MapPart>>,
    meshes: Res<Assets<Mesh>>,
    standard: Res<Assets<StandardMaterial>>,
    mut materials: ResMut<Assets<EnvMaterial>>,
    mut water_materials: ResMut<Assets<WaterMaterial>>,
    mut sky_materials: ResMut<Assets<SkyMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(mut env) = env else { return };
    let env = &mut *env;
    for (entity, material, mesh3d, meta) in &added {
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
        if model.starts_with('o') {
            let sky = env
                .skies
                .entry(model.clone())
                .or_insert_with(|| {
                    let image = sky_texture(&disc.0, &mut env.packs, &model)?;
                    let handle = images.add(image);
                    info!("{model}: Map_Sky dome");
                    Some(sky_materials.add(make_sky_material(
                        handle,
                        AlphaMode::Opaque,
                        true,
                        false,
                        1.0,
                    )))
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
            if !env.water_y_from_mesh {
                if let Some(aabb) = mesh3d
                    .and_then(|m| meshes.get(&m.0))
                    .and_then(|m| m.compute_aabb())
                {
                    let world_y = part_xf.transform_point(Vec3::from(aabb.center)).y;
                    env.water_y = world_y;
                    env.water_y_from_mesh = true;
                }
            }
            if !env.refl_camera_spawned {
                env.refl_camera_spawned = true;
                let far = env.env.scene.as_ref().map_or(15000.0, |s| s.far);
                commands.spawn((
                    WaterReflectionCamera,
                    Camera3d::default(),
                    Camera {
                        order: -1,
                        ..default()
                    },
                    RenderTarget::Image(env.refl_tex.clone().into()),
                    Hdr,
                    Tonemapping::None,
                    RenderLayers::layer(0),
                    Projection::Perspective(PerspectiveProjection { far, ..default() }),
                    Transform::default(),
                ));
                info!(
                    "water planar reflection camera spawned (y={:.2}, {}x{})",
                    env.water_y, WATER_REFL_DEFAULT_W, WATER_REFL_DEFAULT_H
                );
            }
            let clear = env
                .env
                .scene
                .as_ref()
                .map_or([37, 50, 49], |s| s.clear);
            let bump_id = base.normal_map_texture.as_ref().map(Handle::id);
            let refl = env.refl_tex.clone();
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
                            params: water_params(set, has_bump, true, clear),
                            bump,
                            refl,
                        },
                    })
                })
                .clone();
            commands
                .entity(entity)
                .remove::<MeshMaterial3d<StandardMaterial>>()
                .insert((
                    MeshMaterial3d(handle),
                    RenderLayers::layer(WATER_RENDER_LAYER),
                ));
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
                        params: params(set, has_d1, has_d2, false, 0.0),
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
        assert!((b.prefilter.threshold - 1.8).abs() < 1e-5);
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
        let wp = water_params(&set, true, true, [37, 50, 49]);
        assert!((wp.spec0.x - 1.2 * 1.2 * 2.0).abs() < 1e-4);
        assert_eq!(wp.fresnel, Vec4::new(70.0, 0.5, 0.9, 10.0));
        assert!((wp.fresnel_color.x - 0.4784314).abs() < 1e-4);
        assert!((wp.fresnel_color.w - 0.2).abs() < 1e-5);
        assert!((wp.water_color.x - 0.0225).abs() < 1e-5);
        assert_eq!(wp.fallback_refl.w, 1.0);
        assert_eq!(wp.scroll2_time.w, 1.0);
        let ep = params(&set, true, true, false, 0.0);
        assert_eq!(ep.flags, Vec4::new(1.0, 1.0, 0.0, 0.0));
    }

    #[test]
    fn planar_reflection_camera_mirrors_pose_and_clips_underwater() {
        let water_y = 2.08;
        let main = Transform::from_xyz(324.65, 36.8, 649.5)
            .looking_at(Vec3::new(500.0, 10.0, 650.0), Vec3::Y);
        let refl = mirror_camera_pose(&main, water_y);

        // Mirrored Y matches 2 * water_y - main.y (`sheets/map_env.csv` fog.c14).
        assert!((refl.translation.x - main.translation.x).abs() < 1e-4);
        assert!((refl.translation.y - (2.0 * water_y - main.translation.y)).abs() < 1e-4);
        assert!((refl.translation.z - main.translation.z).abs() < 1e-4);

        // Rotation is proper right-handed (det = +1) so backface winding is preserved.
        let basis = Mat3::from_quat(refl.rotation);
        assert!((basis.determinant() - 1.0).abs() < 1e-4);

        // Oblique clip plane keeps points above water_y and clips points below it.
        let plane = oblique_water_clip_plane(&refl, water_y);
        let above_view = refl.rotation.inverse() * (Vec3::new(400.0, 12.0, 650.0) - refl.translation);
        let below_view = refl.rotation.inverse() * (Vec3::new(400.0, -8.9, 650.0) - refl.translation);
        assert!(plane.truncate().dot(above_view) + plane.w > 0.0);
        assert!(plane.truncate().dot(below_view) + plane.w < 0.0);
    }
}
