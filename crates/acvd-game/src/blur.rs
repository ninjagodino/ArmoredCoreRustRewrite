//! Fullscreen post-processing pass on the main 3D camera:
//! - Speed zoom blur (`sheets/camera_follow.csv` rows `speed_blur` / `speed_blur_draw`, 360
//!   `0x82c97ff8` / `0x82c98368`).
//! - Crepuscular god rays (`sheets/map_env.csv` row `god_ray`, 360 `0x82c9f9f0`).
//! - `m_ToneMapFilter` (`sheets/map_env.csv` row `tonemap`, 360 `0x82c18628`): `c31 = 0.5`
//!   halving + Divide/Reinhard/Exp/Filmic tonemapping applied once after HDR `Bloom` across the
//!   entire composited 3D frame (sortie maps, garage workshops, sky domes, AC parts, spotlights,
//!   and VFX).

use bevy::asset::embedded_asset;
use bevy::core_pipeline::fullscreen_material::{FullscreenMaterial, FullscreenMaterialPlugin};
use bevy::core_pipeline::tonemapping::tonemapping;
use bevy::core_pipeline::Core3dSystems;
use bevy::ecs::schedule::ScheduleConfigs;
use bevy::ecs::system::BoxedSystem;
use bevy::post_process::bloom::bloom;
use bevy::prelude::*;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::ShaderType;
use bevy::shader::ShaderRef;

/// Width of the frame the game blurs (720p); BlurOffset is in its pixels (360 0x82c97ff8 divides
/// it by the source texture's width).
const FRAME_WIDTH: f32 = 1280.0;

/// Sentinel tonemap vector that leaves pixels untouched when no `_env.msb` is active.
pub const TONEMAP_PASSTHROUGH: Vec4 = Vec4::new(0.0, 1.0, 10.0, 99.0);

/// Active 360 `m_ToneMapFilter` (`0x82c18628`) constants `(c32 = 1/(lum*key), c33 = mul,
/// c34 = white, mode)` synced onto every main camera's [`ZoomBlur`] each frame in `PostUpdate`.
#[derive(Resource, Clone, Copy, Debug)]
pub struct ActiveToneMap(pub Vec4);

impl Default for ActiveToneMap {
    fn default() -> Self {
        Self(TONEMAP_PASSTHROUGH)
    }
}

/// Garage menu backdrop post-process parameters synced onto every main camera's [`ZoomBlur`]:
/// `(blur_strength, dim_factor, grid_alpha, grid_spacing_720p)`. All zeros disables the pass.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct ActiveMenuBackdrop(pub Vec4);

/// Speed blur + crepuscular god-ray + `m_ToneMapFilter` parameters for one camera (`ZoomBlur`
/// 360 `0x82c97ff8` / `0x82c98368`, `LensFlareGodRay` 360 `0x82c9f9f0`, `m_ToneMapFilter` 360
/// `0x82c18628`), combined into a single `FullscreenMaterial` pass running deterministically
/// after HDR `Bloom` and before Bevy's `tonemapping` (which is `Tonemapping::None`).
#[derive(Component, ExtractComponent, Clone, Copy, ShaderType)]
pub struct ZoomBlur {
    /// Tap spacing in UV per unit of distance from the centre: BlurOffset x intensity / width.
    step: f32,
    /// Blend toward the blurred colour: BlurAlpha x intensity / 255.
    alpha: f32,
    /// Edge weight of the ramp: BlurThinPow x intensity.
    thin: f32,
    /// BlurNoEffectSizeX / Y: the centre rectangle that is copied unblurred, as a share of the screen.
    no_effect: Vec2,
    /// Crepuscular god-ray radial blur + merge (`LensFlareGodRay*PS`, updated each frame in
    /// `PostUpdate` by `lens_flare::update_lens_flare`).
    pub god_ray: crate::lens_flare::GodRayFilter,
    /// `m_ToneMapFilter` (`0x82c18628`): `(c32 = 1 / (lum * key), c33 = mul, c34 = white, mode)`.
    /// Mode `> 10` or `c32 <= 0` passes through untouched.
    pub tonemap: Vec4,
    /// Garage menu blurred bokeh + `+` reticle grid backdrop:
    /// `(blur_strength, dim_factor, grid_alpha, grid_spacing_720p)`.
    pub menu_backdrop: Vec4,
}

impl Default for ZoomBlur {
    fn default() -> Self {
        Self {
            step: 0.0,
            alpha: 0.0,
            thin: 0.0,
            no_effect: Vec2::ZERO,
            god_ray: crate::lens_flare::GodRayFilter::default(),
            tonemap: TONEMAP_PASSTHROUGH,
            menu_backdrop: Vec4::ZERO,
        }
    }
}

impl ZoomBlur {
    /// Updates only the speed zoom-blur fields, preserving `god_ray` and `tonemap`.
    pub fn set_speed_blur(&mut self, offset: f32, alpha: u8, thin: f32, no_effect: Vec2) {
        self.step = offset / FRAME_WIDTH;
        self.alpha = f32::from(alpha) / 255.0;
        self.thin = thin;
        self.no_effect = no_effect;
    }

    /// Clears only the speed zoom-blur fields, preserving `god_ray` and `tonemap`.
    pub fn clear_speed_blur(&mut self) {
        self.step = 0.0;
        self.alpha = 0.0;
        self.thin = 0.0;
        self.no_effect = Vec2::ZERO;
    }
}

impl FullscreenMaterial for ZoomBlur {
    fn fragment_shader() -> ShaderRef {
        "embedded://acvd_game/zoom_blur.wgsl".into()
    }

    fn schedule_configs(system: ScheduleConfigs<BoxedSystem>) -> ScheduleConfigs<BoxedSystem> {
        system
            .in_set(Core3dSystems::PostProcess)
            .after(bloom)
            .before(tonemapping)
    }
}

fn sync_tonemap(
    active: Res<ActiveToneMap>,
    backdrop: Res<ActiveMenuBackdrop>,
    mut cams: Query<&mut ZoomBlur>,
) {
    for mut blur in &mut cams {
        blur.tonemap = active.0;
        blur.menu_backdrop = backdrop.0;
    }
}

pub struct BlurPlugin;

impl Plugin for BlurPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "zoom_blur.wgsl");
        app.init_resource::<ActiveToneMap>()
            .init_resource::<ActiveMenuBackdrop>()
            .add_plugins(FullscreenMaterialPlugin::<ZoomBlur>::default())
            .add_systems(PostUpdate, sync_tonemap);
    }
}
