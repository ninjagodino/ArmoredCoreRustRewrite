//! Speed blur: the zoom-blur filter the follow camera drives from the AC's speed
//! (`sheets/camera_follow.csv` rows `speed_blur` / `speed_blur_draw`).

use bevy::asset::embedded_asset;
use bevy::core_pipeline::fullscreen_material::{FullscreenMaterial, FullscreenMaterialPlugin};
use bevy::prelude::*;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::ShaderType;
use bevy::shader::ShaderRef;

/// Width of the frame the game blurs (720p); BlurOffset is in its pixels (360 0x82c97ff8 divides
/// it by the source texture's width).
const FRAME_WIDTH: f32 = 1280.0;

/// Zoom-blur parameters for one camera, as 360 0x82c97ff8 / 0x82c98368 hand them to the shaders.
#[derive(Component, ExtractComponent, Clone, Copy, ShaderType, Default)]
pub struct ZoomBlur {
    /// Tap spacing in UV per unit of distance from the centre: BlurOffset x intensity / width.
    step: f32,
    /// Blend toward the blurred colour: BlurAlpha x intensity / 255.
    alpha: f32,
    /// Edge weight of the ramp: BlurThinPow x intensity.
    thin: f32,
    /// BlurNoEffectSizeX / Y: the centre rectangle that is copied unblurred, as a share of the screen.
    no_effect: Vec2,
}

impl ZoomBlur {
    pub fn new(offset: f32, alpha: u8, thin: f32, no_effect: Vec2) -> Self {
        Self {
            step: offset / FRAME_WIDTH,
            alpha: f32::from(alpha) / 255.0,
            thin,
            no_effect,
        }
    }
}

impl FullscreenMaterial for ZoomBlur {
    fn fragment_shader() -> ShaderRef {
        "embedded://acvd_game/zoom_blur.wgsl".into()
    }
}

pub struct BlurPlugin;

impl Plugin for BlurPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "zoom_blur.wgsl");
        app.add_plugins(FullscreenMaterialPlugin::<ZoomBlur>::default());
    }
}
