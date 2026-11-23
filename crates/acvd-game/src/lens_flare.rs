//! ACVD Lens Flare, Dirty-Lens Dust (`lfdust`), Crepuscular God Rays (`LensFlareGodRay*PS`),
//! and Dynamic World-Space Point Flares (`AppFxLensFlareDrawEntity`).
//!
//! 360 references (`sheets/map_env.csv` rows `lens_flare`, `god_ray`, `lens_dust`):
//! - `0x827ffa80` -> `0x827fc548` -> `0x82800350`: unpacks type-101 sub-block 3 into
//!   `m_LensFlare` (`0x83a8b220`, vtable `0x821036c0`).
//! - `0x82ca1a78`: loads `model/filter/lensflare/lensflare_t.bnd|lensflare.tpf` (`lf00`..`lf10`
//!   + `lfdust`).
//! - `0x82ca1ef8` / `0x82ca0ea8`: builds sun direction from pitch/yaw and projects to screen.
//! - `0x82ca2918`: smooth occlusion query readback.
//! - `0x82c9f5b0`: 4 primary glare/streak slots at the sun.
//! - `0x82ca0370`: 18 ghost chain slots along `sun + (opposite - sun) * axis_pos`.
//! - `0x82ca0800` / `0x82ca1510`: `lfdust` 512x512 dirty-lens overlay in 720p coordinates.
//! - `0x82c9f9f0`: crepuscular god-ray radial blur (`LensFlareGodRay{12,16,20}PS.fpo`) +
//!   merge (`LensFlareGodRayMerge{Add,Modulate,Transparent}PS.fpo`).
//! - `0x82c7b2b0`: `AppFxLensFlareDrawEntity` dynamic world-space point flares.

use acvd_formats::env::{GodRay, LensFlare};
use acvd_formats::tpf;
use acvd_formats::vfs::{self, Disc};
use acvd_render::Packs;
use bevy::asset::embedded_asset;
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, RenderPipelineDescriptor,
    ShaderType,
};
use bevy::shader::ShaderRef;
use bevy::ui::UiTransform;
use bevy::ui_render::prelude::{MaterialNode, UiMaterial, UiMaterialPlugin};
use bevy::ui_render::ui_material::UiMaterialKey;

use crate::blur::ZoomBlur;
use crate::collision::{Collision, RayFilter};
use crate::{Ac, Garage};

/// 360 reference screen resolution used by `m_LensFlare` (`0x82ca0e54` / `0x82ca0e84` / `0x82ca0800`).
const REF_W: f32 = 1280.0;
const REF_H: f32 = 720.0;

const LENSFLARE_TPF: &str = "model/filter/lensflare/lensflare_t.bnd|lensflare.tpf";

const MAX_GLARES: usize = 4;
const MAX_GHOSTS: usize = 18;
const MAX_POINT_FLARES: usize = 8;

/// Sprite pool layout:
/// - `0..4`: 4 primary glare slots (`0x82c9f5b0`)
/// - `4`: optional anamorphic streak (`lf09`) when not already in the map's glare list
/// - `5..23`: 18 ghost chain slots (`0x82ca0370`)
/// - `23`: `lfdust` dirty-lens overlay (`0x82ca1510`)
/// - `24..48`: `MAX_POINT_FLARES * 3` dynamic `AppFxLensFlareDrawEntity` slots (core, streak, ghost)
const STREAK_SLOT: usize = MAX_GLARES;
const GHOST_BASE: usize = STREAK_SLOT + 1;
const DUST_SLOT: usize = GHOST_BASE + MAX_GHOSTS;
const POINT_BASE: usize = DUST_SLOT + 1;
const POOL_SIZE: usize = POINT_BASE + MAX_POINT_FLARES * 3;

const ADDITIVE_ONE: BlendState = BlendState {
    color: BlendComponent {
        src_factor: BlendFactor::One,
        dst_factor: BlendFactor::One,
        operation: BlendOperation::Add,
    },
    alpha: BlendComponent::OVER,
};

/// Active map/garage lens-flare configuration, populated by `env::apply_env_file`.
#[derive(Resource, Clone, Default)]
pub struct ActiveLensFlare {
    pub config: LensFlare,
    pub dirty: bool,
}

impl ActiveLensFlare {
    pub fn set(&mut self, config: LensFlare) {
        self.config = config;
        self.dirty = true;
    }

    pub fn clear(&mut self) {
        self.config = LensFlare::default();
        self.dirty = true;
    }
}

/// Installs an `_env.msb`'s `LensFlare` config into [`ActiveLensFlare`].
pub fn set_active(commands: &mut Commands, config: LensFlare) {
    commands.queue(move |world: &mut World| {
        if let Some(mut active) = world.get_resource_mut::<ActiveLensFlare>() {
            active.set(config);
        }
    });
}

/// Clears [`ActiveLensFlare`] (e.g. when leaving a sortie map for the 3D garage).
pub fn clear_active(commands: &mut Commands) {
    commands.queue(|world: &mut World| {
        if let Some(mut active) = world.get_resource_mut::<ActiveLensFlare>() {
            active.clear();
        }
        if let Some(mut q) = world.get_resource_mut::<PointFlareQueue>() {
            q.active.clear();
        }
    });
}

/// Spawns a dynamic world-space lens flare (`AppFxLensFlareDrawEntity`, 360 `0x82c7b2b0`)
/// for bright gameplay events (explosions, Overed Weapon discharges, destruction flashes).
pub fn spawn_point_flare(
    commands: &mut Commands,
    pos: Vec3,
    color: Vec3,
    intensity: f32,
    scale: f32,
    duration: f32,
) {
    commands.queue(move |world: &mut World| {
        if let Some(mut q) = world.get_resource_mut::<PointFlareQueue>() {
            q.spawn(pos, color, intensity, scale, duration);
        }
    });
}

/// Crepuscular god-ray post-process uniform (`zoom_blur.wgsl`, 360 `0x82c9f9f0`).
#[derive(Clone, Copy, ShaderType, Default, Debug)]
pub struct GodRayFilter {
    pub sun_uv: Vec2,
    pub density: f32,
    pub decay: f32,
    pub weight: f32,
    pub exposure: f32,
    pub intensity: f32,
    pub emitter_radius: f32,
    /// `xyz = merge_color.rgb / 255`, `w = merge_mode` (0 Add, 1 Modulate, 2 Transparent).
    pub merge_color: Vec4,
    /// `xyz = emitter_color.rgb / 255`, `w = sample_count` (12, 16, or 20).
    pub emitter_color: Vec4,
}

impl GodRayFilter {
    /// Builds the shader uniform from the 360 `_env.msb` `GodRay` block, sun screen UV, and
    /// smooth visibility (`0.0..=1.0`). Returns an inactive (`intensity = 0`) filter when
    /// `tex_id < 0` or `visibility <= 0`.
    pub fn from_env(gr: &GodRay, sun_uv: Vec2, visibility: f32) -> Self {
        if gr.tex_id < 0 || visibility <= 0.0001 || gr.merge_intensity <= 0.0001 {
            return Self::default();
        }
        let samples = match gr.sample_mode {
            0 => 12.0,
            1 => 16.0,
            _ => 20.0,
        };
        let emitter_radius = (0.14 * gr.emitter_scale.max(0.25)).clamp(0.05, 0.55);
        Self {
            sun_uv,
            density: gr.blur_params[0].clamp(0.0, 2.0),
            decay: gr.blur_params[1].clamp(0.5, 0.999),
            weight: gr.blur_params[2].clamp(0.0, 2.0),
            exposure: gr.blur_params[3].clamp(0.0, 4.0),
            intensity: (gr.merge_intensity * visibility).clamp(0.0, 2.0),
            emitter_radius,
            merge_color: Vec4::new(
                gr.merge_color[0] as f32 / 255.0,
                gr.merge_color[1] as f32 / 255.0,
                gr.merge_color[2] as f32 / 255.0,
                (gr.merge_mode.min(2)) as f32,
            ),
            emitter_color: Vec4::new(
                gr.emitter_color[0] as f32 / 255.0,
                gr.emitter_color[1] as f32 / 255.0,
                gr.emitter_color[2] as f32 / 255.0,
                samples,
            ),
        }
    }
}

/// Additive UI material for `lf00`..`lf10` and `lfdust` (`lens_flare.wgsl`).
#[derive(AsBindGroup, Asset, TypePath, Clone)]
pub struct LensFlareMat {
    #[uniform(0)]
    pub tint: LinearRgba,
    #[texture(1)]
    #[sampler(2)]
    pub image: Handle<Image>,
}

impl UiMaterial for LensFlareMat {
    fn fragment_shader() -> ShaderRef {
        "embedded://acvd_game/lens_flare.wgsl".into()
    }

    fn specialize(descriptor: &mut RenderPipelineDescriptor, _key: UiMaterialKey<Self>) {
        if let Some(fragment) = descriptor.fragment.as_mut() {
            for target in fragment.targets.iter_mut().flatten() {
                target.blend = Some(ADDITIVE_ONE);
            }
        }
    }
}

/// Decoded `lf00`..`lf10` and `lfdust` textures from `lensflare_t.bnd|lensflare.tpf`.
#[derive(Resource)]
struct LensFlareAssets {
    sprites: [Option<(Handle<Image>, Vec2)>; 11],
    dust: Option<(Handle<Image>, Vec2)>,
}

/// Runtime state of smooth sun occlusion and PRNG flicker (`0x82ca2918`, `0x82c9f7a0`).
#[derive(Resource)]
struct SunFlareState {
    visibility: f32,
    settled: bool,
    rng: u32,
}

impl Default for SunFlareState {
    fn default() -> Self {
        Self {
            visibility: 0.0,
            settled: false,
            rng: 0x1234_5678,
        }
    }
}

impl SunFlareState {
    /// Matches 360's LCG (`mullw * 0x41c64e6d + 0x3039`, `0x82c9f7a4`).
    fn next_flicker(&mut self, amplitude: f32) -> f32 {
        if amplitude <= 0.0 {
            return 1.0;
        }
        self.rng = self.rng.wrapping_mul(0x41c6_4e6d).wrapping_add(0x3039);
        let r = ((self.rng >> 16) & 0x7fff) as f32 / 32767.0;
        (1.0 + (r * 2.0 - 1.0) * amplitude).max(0.0)
    }
}

/// A dynamic world-space lens flare (`AppFxLensFlareDrawEntity`, 360 `0x82c7b2b0`):
/// explosions, Overed Weapon discharges, and bright light flashes.
#[derive(Clone, Debug)]
pub struct PointFlare {
    pub pos: Vec3,
    pub color: Vec3,
    pub intensity: f32,
    pub scale: f32,
    pub age: f32,
    pub duration: f32,
}

impl PointFlare {
    /// Remaining intensity envelope (fast attack, smooth quadratic fade).
    pub fn envelope(&self) -> f32 {
        if self.duration <= 0.0 || self.age >= self.duration {
            return 0.0;
        }
        let t = (self.age / self.duration).clamp(0.0, 1.0);
        let fade = 1.0 - t;
        self.intensity * fade * fade
    }
}

/// Queue of active dynamic world-space point flares (`AppFxLensFlareDrawEntity`).
#[derive(Resource, Default)]
pub struct PointFlareQueue {
    pub active: Vec<PointFlare>,
}

impl PointFlareQueue {
    /// Spawns a dynamic world-space flare (explosion, Overed Weapon discharge, flash).
    pub fn spawn(&mut self, pos: Vec3, color: Vec3, intensity: f32, scale: f32, duration: f32) {
        if intensity <= 0.0 || duration <= 0.0 {
            return;
        }
        if self.active.len() >= MAX_POINT_FLARES {
            // Replace the oldest / weakest flare.
            if let Some((idx, _)) = self
                .active
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| a.envelope().total_cmp(&b.envelope()))
            {
                self.active.swap_remove(idx);
            }
        }
        self.active.push(PointFlare {
            pos,
            color,
            intensity,
            scale,
            age: 0.0,
            duration,
        });
    }

    pub fn tick(&mut self, dt: f32) {
        for f in &mut self.active {
            f.age += dt;
        }
        self.active.retain(|f| f.age < f.duration);
    }
}

#[derive(Component)]
struct FlareSlot(usize);

#[derive(Resource)]
struct FlareDisc(Disc);

pub struct LensFlarePlugin {
    pub disc: Disc,
}

impl Plugin for LensFlarePlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "lens_flare.wgsl");
        app.insert_resource(FlareDisc(self.disc.clone()))
            .init_resource::<ActiveLensFlare>()
            .init_resource::<SunFlareState>()
            .init_resource::<PointFlareQueue>()
            .add_plugins(UiMaterialPlugin::<LensFlareMat>::default())
            .add_systems(Startup, setup_lens_flare)
            .add_systems(
                PostUpdate,
                update_lens_flare.after(TransformSystems::Propagate),
            );
    }
}

fn load_assets(disc: &Disc, images: &mut Assets<Image>) -> LensFlareAssets {
    let mut sprites: [Option<(Handle<Image>, Vec2)>; 11] = Default::default();
    let mut dust = None;

    let Ok(bytes) = vfs::open(disc, LENSFLARE_TPF) else {
        warn!("lens_flare: could not open {LENSFLARE_TPF}");
        return LensFlareAssets { sprites, dust };
    };
    let Ok(header) = tpf::read(&bytes) else {
        warn!("lens_flare: could not parse {LENSFLARE_TPF}");
        return LensFlareAssets { sprites, dust };
    };

    let mut packs = Packs::default();
    for (idx, tex) in header.textures.iter().enumerate() {
        let lower = tex.name.to_ascii_lowercase();
        let Ok(img) = packs.texture_at(disc, LENSFLARE_TPF, idx, &tex.name) else {
            continue;
        };
        let size = Vec2::new(tex.width as f32, tex.height as f32);
        let handle = images.add(img);
        if lower == "lfdust" {
            dust = Some((handle, size));
        } else if let Some(rest) = lower.strip_prefix("lf") {
            if let Ok(n) = rest.parse::<usize>() {
                if n < sprites.len() {
                    sprites[n] = Some((handle, size));
                }
            }
        }
    }
    let loaded_sprites = sprites.iter().filter(|s| s.is_some()).count();
    info!(
        "lens_flare: loaded {loaded_sprites}/11 sprites, dust={}",
        dust.is_some()
    );
    LensFlareAssets { sprites, dust }
}

fn setup_lens_flare(
    mut commands: Commands,
    disc: Res<FlareDisc>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<LensFlareMat>>,
) {
    let assets = load_assets(&disc.0, &mut images);
    let fallback_img = assets
        .sprites
        .iter()
        .flatten()
        .next()
        .map(|(h, _)| h.clone())
        .unwrap_or_default();

    // Full-screen overlay behind the HUD (`GlobalZIndex(-10)`), ignoring pointer events.
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                overflow: Overflow::clip(),
                ..default()
            },
            GlobalZIndex(-10),
            Pickable::IGNORE,
        ))
        .with_children(|root| {
            for slot in 0..POOL_SIZE {
                let mat = mats.add(LensFlareMat {
                    tint: LinearRgba::new(0.0, 0.0, 0.0, 0.0),
                    image: fallback_img.clone(),
                });
                root.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(0.0),
                        top: Val::Px(0.0),
                        width: Val::Px(0.0),
                        height: Val::Px(0.0),
                        ..default()
                    },
                    UiTransform::IDENTITY,
                    MaterialNode(mat),
                    Visibility::Hidden,
                    Pickable::IGNORE,
                    FlareSlot(slot),
                ));
            }
        });

    commands.insert_resource(assets);
}

/// Ray-AABB slab intersection test for player AC self-occlusion.
pub(crate) fn ray_hits_aabb(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3, max_t: f32) -> bool {
    let mut tmin = 0.0_f32;
    let mut tmax = max_t;
    for axis in 0..3 {
        let o = origin[axis];
        let d = dir[axis];
        if d.abs() < 1e-6 {
            if o < min[axis] || o > max[axis] {
                return false;
            }
        } else {
            let inv = 1.0 / d;
            let mut t1 = (min[axis] - o) * inv;
            let mut t2 = (max[axis] - o) * inv;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
            }
            tmin = tmin.max(t1);
            tmax = tmax.min(t2);
            if tmin > tmax {
                return false;
            }
        }
    }
    tmax >= 0.0 && tmin <= max_t
}

/// Computes the mirrored ghost position along the sun-to-opposite axis (`0x82ca0500..0x82ca052c`):
/// `opposite = 2 * centre - sun`, `pos = sun + (opposite - sun) * axis_pos`.
pub(crate) fn ghost_axis_pos(sun: Vec2, centre: Vec2, axis_pos: f32) -> Vec2 {
    let opposite = centre * 2.0 - sun;
    sun + (opposite - sun) * axis_pos
}

/// Smoothly fades flare visibility near and just past the screen border.
fn edge_fade(uv: Vec2) -> f32 {
    let dx = (uv.x - 0.5).abs();
    let dy = (uv.y - 0.5).abs();
    let m = dx.max(dy);
    if m <= 0.45 {
        1.0
    } else if m >= 0.72 {
        0.0
    } else {
        1.0 - (m - 0.45) / (0.72 - 0.45)
    }
}

/// 5-ray disc occlusion test against map `.hmd` BVH (`Collision`) and the player AC's AABB.
/// Returns `(unoccluded_fraction, world_hits, ac_hits)`.
fn sun_unoccluded_fraction(
    cam_pos: Vec3,
    sun_dir: Vec3,
    collision: Option<&Collision>,
    ac_aabb: Option<(Vec3, Vec3)>,
) -> (f32, u32, u32) {
    let up = if sun_dir.y.abs() < 0.95 {
        Vec3::Y
    } else {
        Vec3::X
    };
    let right = sun_dir.cross(up).normalize_or_zero();
    let bitangent = right.cross(sun_dir).normalize_or_zero();
    // ~1.2 degree solar disc half-angle for smooth partial occlusion behind cover edges.
    let spread = 0.021_f32;
    let offsets = [
        Vec3::ZERO,
        right * spread,
        -right * spread,
        bitangent * spread,
        -bitangent * spread,
    ];
    let ray_len = 4500.0;
    let mut open = 0u32;
    let mut world_hits = 0u32;
    let mut ac_hits = 0u32;
    for off in offsets {
        let dir = (sun_dir + off).normalize();
        let hit_world = collision
            .and_then(|c| c.raycast(cam_pos, cam_pos + dir * ray_len, RayFilter::FcsLos))
            .is_some();
        let hit_ac = ac_aabb.is_some_and(|(mn, mx)| ray_hits_aabb(cam_pos, dir, mn, mx, 60.0));
        if hit_world {
            world_hits += 1;
        }
        if hit_ac {
            ac_hits += 1;
        }
        if !hit_world && !hit_ac {
            open += 1;
        }
    }
    (open as f32 / offsets.len() as f32, world_hits, ac_hits)
}

#[allow(clippy::too_many_arguments)]
fn update_lens_flare(
    time: Res<Time>,
    mut active: ResMut<ActiveLensFlare>,
    mut state: ResMut<SunFlareState>,
    mut point_flares: ResMut<PointFlareQueue>,
    assets: Option<Res<LensFlareAssets>>,
    collision: Option<Res<Collision>>,
    garage: Option<Res<Garage>>,
    shot_mark: Option<Res<acvd_render::app::ShotMark>>,
    ac_q: Query<&GlobalTransform, With<Ac>>,
    mut cam_q: Query<
        (&Camera, &GlobalTransform, Option<&mut ZoomBlur>),
        (
            With<Camera3d>,
            Without<crate::env::WaterReflectionCamera>,
            Without<crate::garage_assemble::PartPreviewCamera>,
        ),
    >,
    mut slots: Query<(
        &FlareSlot,
        &mut Node,
        &mut UiTransform,
        &mut Visibility,
        &MaterialNode<LensFlareMat>,
    )>,
    mut mats: ResMut<Assets<LensFlareMat>>,
) {
    let dt = time.delta_secs();
    point_flares.tick(dt);

    let Some(assets) = assets else {
        return;
    };
    let Ok((camera, cam_gt, mut god_ray_filter)) = cam_q.single_mut() else {
        return;
    };
    let Some(vp_size) = camera.logical_viewport_size() else {
        return;
    };
    if vp_size.x <= 1.0 || vp_size.y <= 1.0 {
        return;
    }

    if active.dirty {
        state.settled = false;
        active.dirty = false;
    }

    // Collect desired draw commands into a fixed array indexed by pool slot.
    struct DrawCmd {
        img: Handle<Image>,
        center: Vec2,
        size: Vec2,
        rot: f32,
        tint: LinearRgba,
    }
    let mut cmds: [Option<DrawCmd>; POOL_SIZE] = std::array::from_fn(|_| None);

    let cam_pos = cam_gt.translation();
    let cam_fwd = cam_gt.forward().as_vec3();
    let screen_centre = vp_size * 0.5;
    let scale_x = vp_size.x / REF_W;
    let scale_y = vp_size.y / REF_H;

    // Player AC world-space AABB for self-occlusion (inset slightly so it hugs the silhouette).
    let ac_aabb = garage.as_ref().and_then(|g| {
        let gt = ac_q.iter().next()?;
        let pos = gt.translation();
        let mn = pos + g.bounds.0 * 0.78;
        let mx = pos + g.bounds.1 * 0.90;
        Some((mn.min(mx), mn.max(mx)))
    });

    // 1. Directional sun lens flare, anamorphic streak, dirty-lens dust, and crepuscular god rays.
    let lf = &active.config;
    let mut god_ray_out = GodRayFilter::default();
    let mut dbg_world_hits = 0u32;
    let mut dbg_ac_hits = 0u32;
    let mut dbg_sun_uv = Vec2::ZERO;

    if lf.enabled {
        let g = lf.direction();
        // Same game -> Bevy axis mirror as `env::bevy_dir`: (-x, y, z).
        let sun_dir = Vec3::new(-g[0], g[1], g[2]).normalize_or_zero();
        let in_front = sun_dir.dot(cam_fwd) > 0.05;

        let sun_screen = if in_front {
            camera
                .world_to_viewport(cam_gt, cam_pos + sun_dir * 2000.0)
                .ok()
        } else {
            None
        };

        let (target_vis, sun_pos, sun_uv) = if let Some(sp) = sun_screen {
            let uv = sp / vp_size;
            dbg_sun_uv = uv;
            let ef = edge_fade(uv);
            let vis = if ef > 0.0 {
                let (frac, wh, ah) =
                    sun_unoccluded_fraction(cam_pos, sun_dir, collision.as_deref(), ac_aabb);
                dbg_world_hits = wh;
                dbg_ac_hits = ah;
                ef * frac
            } else {
                0.0
            };
            (vis, sp, uv)
        } else {
            (0.0, screen_centre, Vec2::splat(0.5))
        };

        if !state.settled {
            state.visibility = target_vis;
            state.settled = true;
        } else {
            let alpha = 1.0 - (-14.0 * dt).exp();
            state.visibility += (target_vis - state.visibility) * alpha;
        }

        let vis = state.visibility;

        // God rays (`0x82c9f9f0`): active when map's `_env.msb` sets `god_ray.tex_id >= 0`.
        if in_front && vis > 0.001 {
            god_ray_out = GodRayFilter::from_env(&lf.god_ray, sun_uv, vis);
        }

        if in_front && vis > 0.001 {
            let to_sun = sun_pos - screen_centre;
            let angle = to_sun.y.atan2(to_sun.x);

            let glare_rgb = Vec3::new(lf.glare_color[0], lf.glare_color[1], lf.glare_color[2]);
            let glare_a = lf.glare_color[3];
            let ghost_rgb = Vec3::new(lf.ghost_color[0], lf.ghost_color[1], lf.ghost_color[2]);
            let ghost_a = lf.ghost_color[3];

            let mut has_lf09 = false;

            // 4 primary glare/streak slots (`0x82c9f5b0`).
            for (i, g_slot) in lf.glares.iter().take(MAX_GLARES).enumerate() {
                let Ok(tid) = usize::try_from(g_slot.tex_id) else {
                    continue;
                };
                if tid == 9 {
                    has_lf09 = true;
                }
                let Some((img, native)) = assets.sprites.get(tid).and_then(|o| o.as_ref()) else {
                    continue;
                };
                let vis_scale = if g_slot.scale_by_vis { vis } else { 1.0 };
                let s = g_slot.scale * vis_scale;
                let size = Vec2::new(native.x * s * scale_x, native.y * s * scale_y);
                let flick = state.next_flicker(lf.glare_flicker);
                let a = (g_slot.alpha * glare_a * vis * flick).clamp(0.0, 1.5);
                if a <= 0.001 || size.x <= 1.0 || size.y <= 1.0 {
                    continue;
                }
                cmds[i] = Some(DrawCmd {
                    img: img.clone(),
                    center: sun_pos,
                    size,
                    rot: if g_slot.rotate { angle } else { 0.0 },
                    tint: LinearRgba::new(glare_rgb.x, glare_rgb.y, glare_rgb.z, a),
                });
            }

            // Subtle horizontal anamorphic streak (`lf09`, 256x32) if not already in the 4 glares.
            if !has_lf09 && !lf.glares.is_empty() {
                if let Some((img, native)) = assets.sprites[9].as_ref() {
                    let s = 2.4 * (0.65 + 0.35 * vis);
                    let size = Vec2::new(native.x * s * scale_x, native.y * s * 0.7 * scale_y);
                    let a = (0.32 * glare_a * vis * vis).clamp(0.0, 1.0);
                    if a > 0.002 {
                        cmds[STREAK_SLOT] = Some(DrawCmd {
                            img: img.clone(),
                            center: sun_pos,
                            size,
                            rot: 0.0,
                            tint: LinearRgba::new(glare_rgb.x, glare_rgb.y, glare_rgb.z, a),
                        });
                    }
                }
            }

            // Up to 18 ghost chain slots (`0x82ca0370`).
            for (i, gh) in lf.ghosts.iter().take(MAX_GHOSTS).enumerate() {
                let Ok(tid) = usize::try_from(gh.tex_id) else {
                    continue;
                };
                let Some((img, native)) = assets.sprites.get(tid).and_then(|o| o.as_ref()) else {
                    continue;
                };
                let pos = ghost_axis_pos(sun_pos, screen_centre, gh.axis_pos);
                let vis_scale = if gh.alt_scale {
                    0.75 + 0.25 * vis
                } else {
                    vis
                };
                let s = gh.scale * vis_scale;
                let size = Vec2::new(native.x * s * scale_x, native.y * s * scale_y);
                let flick = state.next_flicker(lf.ghost_flicker);
                let a = (gh.alpha * ghost_a * vis * flick).clamp(0.0, 1.5);
                if a <= 0.001 || size.x <= 1.0 || size.y <= 1.0 {
                    continue;
                }
                cmds[GHOST_BASE + i] = Some(DrawCmd {
                    img: img.clone(),
                    center: pos,
                    size,
                    rot: if gh.rotate { angle } else { 0.0 },
                    tint: LinearRgba::new(ghost_rgb.x, ghost_rgb.y, ghost_rgb.z, a),
                });
            }

            // Dirty-lens dust overlay (`lfdust`, `0x82ca1510`): positioned in 720p screen space.
            if let Some((dust_img, _)) = assets.dust.as_ref() {
                let da = lf.dust.color[3] as f32 / 255.0;
                if da > 0.001 {
                    let pos = Vec2::new(lf.dust.pos[0] * scale_x, lf.dust.pos[1] * scale_y);
                    let size = Vec2::new(lf.dust.size[0] * scale_x, lf.dust.size[1] * scale_y);
                    // Subtle optical dust intensity modulated by flare visibility.
                    let intensity = (da * vis * 0.22).clamp(0.0, 0.5);
                    if intensity > 0.002 && size.x > 1.0 && size.y > 1.0 {
                        cmds[DUST_SLOT] = Some(DrawCmd {
                            img: dust_img.clone(),
                            center: pos + size * 0.5,
                            size,
                            rot: 0.0,
                            tint: LinearRgba::new(
                                lf.dust.color[0] as f32 / 255.0,
                                lf.dust.color[1] as f32 / 255.0,
                                lf.dust.color[2] as f32 / 255.0,
                                intensity,
                            ),
                        });
                    }
                }
            }
        }
    } else {
        state.visibility = 0.0;
        state.settled = true;
    }

    if let Some(ref mut blur) = god_ray_filter {
        blur.god_ray = god_ray_out;
    }

    // 2. Dynamic world-space point flares (`AppFxLensFlareDrawEntity`, 360 `0x82c7b2b0`).
    for (idx, pf) in point_flares.active.iter().take(MAX_POINT_FLARES).enumerate() {
        let env = pf.envelope();
        if env <= 0.002 {
            continue;
        }
        let to_flare = pf.pos - cam_pos;
        let dist = to_flare.length();
        if dist < 1.0 || to_flare.dot(cam_fwd) <= 0.0 {
            continue;
        }
        let Ok(sp) = camera.world_to_viewport(cam_gt, pf.pos) else {
            continue;
        };
        let ef = edge_fade(sp / vp_size);
        if ef <= 0.001 {
            continue;
        }
        // World BVH occlusion check so explosions behind cover do not flare through walls.
        if let Some(c) = collision.as_deref() {
            let end = cam_pos + to_flare * ((dist - 1.5).max(0.1) / dist);
            if c.raycast(cam_pos, end, RayFilter::FcsLos).is_some() {
                continue;
            }
        }
        let dist_scale = (45.0 / (dist + 18.0)).clamp(0.25, 2.2) * pf.scale;
        let a = (env * ef).clamp(0.0, 1.5);
        let base = POINT_BASE + idx * 3;

        // Core glare (`lf00`).
        if let Some((img, native)) = assets.sprites[0].as_ref() {
            cmds[base] = Some(DrawCmd {
                img: img.clone(),
                center: sp,
                size: Vec2::new(
                    native.x * dist_scale * scale_x,
                    native.y * dist_scale * scale_y,
                ),
                rot: 0.0,
                tint: LinearRgba::new(pf.color.x, pf.color.y, pf.color.z, a * 0.75),
            });
        }
        // Anamorphic horizontal streak (`lf09`).
        if let Some((img, native)) = assets.sprites[9].as_ref() {
            cmds[base + 1] = Some(DrawCmd {
                img: img.clone(),
                center: sp,
                size: Vec2::new(
                    native.x * dist_scale * 2.2 * scale_x,
                    native.y * dist_scale * 0.9 * scale_y,
                ),
                rot: 0.0,
                tint: LinearRgba::new(pf.color.x, pf.color.y, pf.color.z, a * 0.65),
            });
        }
        // Subtle mirrored ghost (`lf02`).
        if let Some((img, native)) = assets.sprites[2].as_ref() {
            let gpos = ghost_axis_pos(sp, screen_centre, 0.72);
            cmds[base + 2] = Some(DrawCmd {
                img: img.clone(),
                center: gpos,
                size: Vec2::new(
                    native.x * dist_scale * 0.8 * scale_x,
                    native.y * dist_scale * 0.8 * scale_y,
                ),
                rot: 0.0,
                tint: LinearRgba::new(pf.color.x, pf.color.y, pf.color.z, a * 0.28),
            });
        }
    }

    // Apply pool draw commands to the UI nodes.
    let mut visible_count = 0usize;
    for (slot, mut node, mut xform, mut vis, mat_node) in &mut slots {
        if let Some(cmd) = cmds[slot.0].take() {
            visible_count += 1;
            node.left = Val::Px(cmd.center.x - cmd.size.x * 0.5);
            node.top = Val::Px(cmd.center.y - cmd.size.y * 0.5);
            node.width = Val::Px(cmd.size.x);
            node.height = Val::Px(cmd.size.y);
            *xform = UiTransform::from_rotation(Rot2::radians(cmd.rot));
            *vis = Visibility::Visible;
            if let Some(mut m) = mats.get_mut(&mat_node.0) {
                m.tint = cmd.tint;
                if m.image != cmd.img {
                    m.image = cmd.img;
                }
            }
        } else if *vis != Visibility::Hidden {
            *vis = Visibility::Hidden;
        }
    }

    if let Some(path) = shot_mark.as_ref().and_then(|m| m.path.as_ref()) {
        println!(
            "SHOT {} lens_flare enabled={} vis={:.3} sprites={} god_ray={:.3} sun_uv=({:.3},{:.3}) world_hits={} ac_hits={}",
            path.display(),
            active.config.enabled,
            state.visibility,
            visible_count,
            god_ray_out.intensity,
            dbg_sun_uv.x,
            dbg_sun_uv.y,
            dbg_world_hits,
            dbg_ac_hits,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghost_axis_matches_360_mirror_formula() {
        let centre = Vec2::new(640.0, 360.0);
        let sun = Vec2::new(940.0, 160.0);
        // axis_pos = 0.0 -> at sun; 0.5 -> screen centre; 1.0 -> mirrored opposite.
        assert!((ghost_axis_pos(sun, centre, 0.0) - sun).length() < 1e-4);
        assert!((ghost_axis_pos(sun, centre, 0.5) - centre).length() < 1e-4);
        let opp = Vec2::new(340.0, 560.0);
        assert!((ghost_axis_pos(sun, centre, 1.0) - opp).length() < 1e-4);
    }

    #[test]
    fn aabb_ray_intersection_occludes_ac() {
        let min = Vec3::new(-2.0, 0.0, -2.0);
        let max = Vec3::new(2.0, 8.0, 2.0);
        // Ray straight through the AC torso hits.
        assert!(ray_hits_aabb(
            Vec3::new(0.0, 4.0, -10.0),
            Vec3::Z,
            min,
            max,
            50.0
        ));
        // Ray aimed above the AC misses.
        assert!(!ray_hits_aabb(
            Vec3::new(0.0, 12.0, -10.0),
            Vec3::Z,
            min,
            max,
            50.0
        ));
    }

    #[test]
    fn god_ray_filter_disabled_when_tex_id_negative() {
        let mut gr = GodRay::default();
        assert_eq!(gr.tex_id, -1);
        let f = GodRayFilter::from_env(&gr, Vec2::new(0.5, 0.3), 1.0);
        assert_eq!(f.intensity, 0.0);

        // Enable with m3210 values (tex_id = 0, sample_mode = 1 -> 16 taps, merge_mode = 2).
        gr.tex_id = 0;
        gr.emitter_scale = 2.0;
        gr.blur_params = [0.3, 0.78, 0.92, 1.0];
        gr.merge_intensity = 1.0;
        gr.sample_mode = 1;
        gr.merge_mode = 2;
        gr.merge_color = [255, 255, 255, 255];
        gr.emitter_color = [10, 10, 10, 15];
        let f2 = GodRayFilter::from_env(&gr, Vec2::new(0.4, 0.25), 0.5);
        assert!((f2.intensity - 0.5).abs() < 1e-4);
        assert!((f2.emitter_color.w - 16.0).abs() < 1e-4);
        assert!((f2.merge_color.w - 2.0).abs() < 1e-4);
    }

    #[test]
    fn point_flare_queue_decays_and_evicts() {
        let mut q = PointFlareQueue::default();
        q.spawn(Vec3::ZERO, Vec3::ONE, 1.0, 1.0, 0.5);
        assert_eq!(q.active.len(), 1);
        let e0 = q.active[0].envelope();
        q.tick(0.25);
        let e1 = q.active[0].envelope();
        assert!(e1 < e0 && e1 > 0.0);
        q.tick(0.3);
        assert!(q.active.is_empty());
    }

    #[test]
    fn lensflare_textures_load_from_disc() {
        let Ok(disc) = Disc::default_for(&vfs::repo_root()) else {
            return;
        };
        let mut images = Assets::<Image>::default();
        let assets = load_assets(&disc, &mut images);
        for (i, slot) in assets.sprites.iter().enumerate() {
            assert!(slot.is_some(), "missing decoded lf{i:02} texture");
        }
        let (_, dust_size) = assets.dust.expect("missing decoded lfdust texture");
        assert_eq!(dust_size, Vec2::new(512.0, 512.0));
    }

    #[test]
    fn sun_occlusion_detects_cover_and_open_sky_from_disc() {
        let Ok(disc) = Disc::default_for(&vfs::repo_root()) else {
            return;
        };
        let env = acvd_formats::env::read(
            &vfs::open(&disc, "model/map/ch_env/m4000_env.msb").unwrap(),
        )
        .unwrap();
        let lf = &env.scene.unwrap().lens_flare;
        let g = lf.direction();
        let sun_dir = Vec3::new(-g[0], g[1], g[2]).normalize();
        let col = crate::collision::load_map(&disc, "m4000").unwrap();
        // Spawn courtyard is shadowed by the building along the sun vector.
        let (frac_cover, world_hits, _) =
            sun_unoccluded_fraction(Vec3::new(324.65, 37.84, 649.50), sun_dir, Some(&col), None);
        assert_eq!(world_hits, 5);
        assert_eq!(frac_cover, 0.0);
        // Open yard 100 m east has an unoccluded sightline to the sun.
        let (frac_open, open_hits, _) =
            sun_unoccluded_fraction(Vec3::new(424.65, 77.84, 649.50), sun_dir, Some(&col), None);
        assert_eq!(open_hits, 0);
        assert_eq!(frac_open, 1.0);
    }
}
