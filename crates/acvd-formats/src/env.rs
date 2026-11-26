//! Map environment (`model/map/ch_env/{map}_env.msb`): the MSB section chain at version
//! `msb::MAGIC_ENV` (VD) or `msb::MAGIC` (ACV), with every record in `EVENT_PARAM_ST`.
//! ACV's type-100 lights (including their compact fog block) use a packed byte layout
//! (`acv_light`); its type-101 core scene, lens flare, screen filter and DOF are decoded by
//! `acv_scene` / `acv_lens_flare`, while the type-101 atmosphere record remains unparsed. Field
//! layouts and the shader constants built from them are in `sheets/map_env.csv`.
//!
//! Event record: `u32 name, i32 id, u32 type, u32 index, u32 data`, offsets relative to the
//! record. Types: 100 light set, 101 scene parameters, 102 wind, 400 BGM (mission envs only).

use anyhow::{ensure, Result};
use serde::Serialize;

use crate::msb;
use crate::reader::Be;

pub const EVENT_LIGHT: u32 = 100;
pub const EVENT_SCENE: u32 = 101;
pub const EVENT_WIND: u32 = 102;
pub const EVENT_BGM: u32 = 400;

/// `POINT_PARAM_ST` dynamic light region types (`0x8280021c` -> `0x827fbb30` -> `0x82be8218`).
pub const POINT_LIGHT: u32 = 1300;
pub const SPOT_LIGHT: u32 = 1310;

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub name: String,
    pub id: i32,
    pub kind: u32,
    pub index: u32,
    /// Absolute offset of the record's data.
    pub data: usize,
}

/// ACV type-100 fog fields with no VD equivalent: the lower alpha bounds and the two
/// range-sign flags. The upper bounds are `Fog::color[3]` and `Fog::height_color[3]`.
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct AcvFog {
    /// Lower distance-fog alpha (`+0x3b` byte / 255).
    pub distance_min: f32,
    /// Lower height-fog alpha (`+0x4b` byte / 255).
    pub height_min: f32,
    /// Nonzero: the runtime negates `1/range` (`+0x3d`).
    pub distance_inverted: bool,
    /// Nonzero: the runtime negates `1/height_range` (`+0x4d`).
    pub height_inverted: bool,
}

/// Fog block of a light set. VD stores it at data `+0xa0..+0xe4`; ACV type-100 uses a
/// compact block (see `acv_light`) and fills [`AcvFog`].
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct Fog {
    /// Distance fog colour; `w` is the fog's maximum (`+0xa0`, ACV `+0x3c` / 255).
    pub color: [f32; 4],
    /// Distance over which distance fog rises (`+0xb0`); ACV stores view-space depth range.
    pub range: f32,
    /// Horizontal distance where distance fog starts (`+0xb4`); ACV stores view-space depth.
    pub start: f32,
    /// Height-fog falloff divisor, clamped to at least 0.1 (`+0xbc`; VD only).
    pub falloff: f32,
    /// Overall fog multiplier on both maxima (`+0xc0`; ACV uses 1).
    pub density: f32,
    /// How much scene luminance tints the fog colours (`+0xc4`; ACV uses 0).
    pub luma_tint: f32,
    /// Nonzero: positive height falloff (`+0xc8`); ACV: `!height_inverted`.
    pub rising: bool,
    /// Height-fog range (`+0xcc`); 0 turns height fog off.
    pub height_range: f32,
    /// World height where height fog starts (`+0xd0`).
    pub height_base: f32,
    /// Height fog colour; `w` is its maximum (`+0xd4`, ACV `+0x4c` / 255).
    pub height_color: [f32; 4],
    /// ACV type-100 extras; `None` for VD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acv: Option<AcvFog>,
}

/// A type-100 event: two directional lights, a hemisphere ambient and a fog block. Draws pick
/// one by `slot`.
#[derive(Debug, Clone, Serialize)]
pub struct LightSet {
    pub name: String,
    /// `group * 32 + id` (data bytes 0 and 1), the key the draw info's light / fog ids select.
    pub slot: u32,
    /// Pitch and yaw in degrees of the two directional lights (`+2`, `+0x28`).
    pub angles: [[i16; 2]; 2],
    /// Directional light colours, RGB and intensity in `w` (`+8`, `+0x2c`).
    pub colors: [[f32; 4]; 2],
    /// Second colour of each light (`+0x18`, `+0x3c`), uploaded to c11 / c12.
    pub colors_b: [[f32; 4]; 2],
    /// Hemisphere ambient: sky (`+0x70`) and ground (`+0x80`) colours, intensity in `w`.
    pub sky: [f32; 4],
    pub ground: [f32; 4],
    /// `+0x90`, uploaded to c39 (unused by the map shader).
    pub extra: [f32; 4],
    pub fog: Fog,
}

impl LightSet {
    /// Unit vector toward light `i`, in game axes: `(-cos p sin y, sin p, -cos p cos y)`.
    pub fn direction(&self, i: usize) -> [f32; 3] {
        let [p, y] = self.angles[i].map(|a| (a as f32).to_radians());
        [-p.cos() * y.sin(), p.sin(), -p.cos() * y.cos()]
    }
}

/// Bloom parameters from type-101 sub-block 2 (`+0x19..+0x3c`, unpacked by `0x827fc428` ->
/// `0x82800450`, executed by `0x82c18580` -> `0x82c17908` bright-pass / `0x82c18308` blur).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Bloom {
    /// `+0x19`: blur/shader selector (`ToneMapFilter +0x28`; m4000 = 3).
    pub iters: u8,
    /// `+0x1a`: nonzero enables the bloom pass (`ToneMapFilter +0x50`, checked at `0x82c1858c`).
    pub enabled: bool,
    /// `+0x1c`: bright-pass luminance threshold (`ToneMapFilter +0x2c`, `0x82c17a48` -> PS c120).
    pub threshold: f32,
    /// `+0x20`: bright-pass offset / range divisor (`ToneMapFilter +0x30`, `0x82c17aac` -> `1/offset`).
    pub offset: f32,
    /// `+0x24`, `+0x28`: horizontal and vertical Gaussian blur scales (`0x82c183d0`, `0x82c184c8`).
    pub blur_scale: [f32; 2],
    /// `+0x2c..+0x38`: bloom tint RGB and scale (`ToneMapFilter +0x40..+0x4c`).
    pub color: [f32; 4],
}

impl Default for Bloom {
    fn default() -> Self {
        Self {
            iters: 3,
            enabled: true,
            threshold: 0.9,
            offset: 0.8,
            blur_scale: [1.0, 1.0],
            color: [1.0; 4],
        }
    }
}

/// Tone-map parameters from type-101 sub-block 2 (`data + u32_be(data + 8)`, `0x827fc428` ->
/// `0x82800450`, executed by `m_ToneMapFilter` at `0x82c18628`).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ToneMap {
    /// `+0`: `0` Exp, `1` Divide (`ToneMap_DivideToneMapPS`), `2` Reinhard, `3` Filmic, `4` RGBM.
    pub mode: u32,
    /// `+4` (`ToneMapFilter +0x10`).
    pub lum: f32,
    /// `+8` (`ToneMapFilter +0x14`, `c33.x`).
    pub mul: f32,
    /// `+0xc` (`ToneMapFilter +0x18`; `c32.x = 1 / (lum * key)`).
    pub key: f32,
    /// `+0x10` (`ToneMapFilter +0x1c`, `c34.x`).
    pub white: f32,
    /// `+0x19..+0x3c`, bloom pass parameters.
    pub bloom: Bloom,
}

impl Default for ToneMap {
    fn default() -> Self {
        Self {
            mode: 1,
            lum: 0.5,
            mul: 1.1,
            key: 0.6,
            white: 10.0,
            bloom: Bloom::default(),
        }
    }
}

/// One of the 4 primary glare/streak sprite slots in type-101 sub-block 3 (`+0x34..+0x44`,
/// unpacked by `0x827fc664..0x827fc820` into `m_LensFlare + 0x4b4`, drawn by `0x82c9f5b0`).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct FlareGlare {
    /// `+0x00`: texture index in `lensflare.tpf` (`lf00`..`lf10`), `< 0` disables the slot.
    pub tex_id: i32,
    /// `+0x04`: `u8` alpha scaled by `1/255`.
    pub alpha: f32,
    /// `+0x08`: sprite size multiplier.
    pub scale: f32,
    /// `+0x0c`: nonzero scales size by the flare visibility factor (`r26+0xa0`).
    pub scale_by_vis: bool,
    /// `+0x0d`: nonzero rotates the sprite toward screen centre (`atan2`).
    pub rotate: bool,
    /// `+0x0e`: mode/blend flag (`r31-0x08`).
    pub mode: u8,
}

/// One of the 18 axis-aligned ghost sprite slots in type-101 sub-block 3 (`+0x44..+0x8c`,
/// unpacked by `0x827fc824..0x827fcc34` into `m_LensFlare + 0x17c`, drawn by `0x82ca0370`).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct FlareGhost {
    /// `+0x00`: texture index in `lensflare.tpf` (`lf00`..`lf10`), `< 0` disables the slot.
    pub tex_id: i32,
    /// `+0x04`: `u8` alpha scaled by `1/255`.
    pub alpha: f32,
    /// `+0x08`: sprite size multiplier.
    pub scale: f32,
    /// `+0x0c`: position along the sun-to-mirrored-opposite axis (`0.0` = sun, `0.5` = centre,
    /// `1.0` = opposite point across screen centre).
    pub axis_pos: f32,
    /// `+0x10`: nonzero rotates the sprite toward screen centre (`atan2`).
    pub rotate: bool,
    /// `+0x11`: nonzero selects the secondary visibility scale (`r30+0xa4` vs `+0xa0`).
    pub alt_scale: bool,
}

/// Crepuscular god-ray radial blur parameters from type-101 sub-block 3 (`+0x8c`, unpacked by
/// `0x827fcc38..0x827fcd38` into `m_LensFlare + 0x564`, executed by `0x82c9f9f0`).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct GodRay {
    /// `+0x00`: emitter texture index (`0xffff` / `< 0` disables god rays).
    pub tex_id: i16,
    /// `+0x04`: emitter sprite size scale (`r30+0x0c`).
    pub emitter_scale: f32,
    /// `+0x08`: emitter depth bias / z param (`r30+0x24`).
    pub emitter_depth_bias: f32,
    /// `+0x0c..+0x0f`: emitter sprite RGBA tint (0..255).
    pub emitter_color: [u8; 4],
    /// `+0x10..+0x1c`: radial-blur PS `c29` (`density, weight, decay, exposure`).
    pub blur_params: [f32; 4],
    /// `+0x20`: merge pass intensity multiplier (`c28.x = merge_intensity * visibility`).
    pub merge_intensity: f32,
    /// `+0x24`: sample-count selector (`0` = 12 taps, `1` = 16 taps, `2` = 20 taps).
    pub sample_mode: u8,
    /// `+0x26`: merge shader selector (`0` = Add, `1` = Modulate, `2` = Transparent).
    pub merge_mode: u8,
    /// `+0x28..+0x2b`: merge tint RGBA (0..255; `.rgb / 255` uploaded to `c28.yzw`).
    pub merge_color: [u8; 4],
}

impl Default for GodRay {
    fn default() -> Self {
        Self {
            tex_id: -1,
            emitter_scale: 1.0,
            emitter_depth_bias: 0.0,
            emitter_color: [0; 4],
            blur_params: [0.0; 4],
            merge_intensity: 0.0,
            sample_mode: 0,
            merge_mode: 0,
            merge_color: [0; 4],
        }
    }
}

/// Dirty-lens dust overlay (`lfdust`) from type-101 sub-block 3 (`+0x90`, unpacked by
/// `0x827fcd3c..0x827fcde8` into `m_LensFlare + 0x5dc`, drawn by `0x82ca1510`).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct LensDust {
    /// `+0x00`, `+0x04`: top-left screen offset in 720p pixels (default `0, 0`).
    pub pos: [f32; 2],
    /// `+0x08`, `+0x0c`: quad size in 720p pixels (default `512, 512`).
    pub size: [f32; 2],
    /// `+0x10..+0x13`: RGBA tint (0..255; default `[255, 255, 255, 0]`).
    pub color: [u8; 4],
}

impl Default for LensDust {
    fn default() -> Self {
        Self {
            pos: [0.0, 0.0],
            size: [512.0, 512.0],
            color: [255, 255, 255, 0],
        }
    }
}

/// Lens-flare, god-ray, and lens-dust block from type-101 sub-block 3 (`data + u32_be(data + 0xc)`,
/// unpacked by `0x827fc548` -> `0x82800350` into `m_LensFlare` at `0x83a8b220`).
///
/// ACV uses the same header layout but shifts the glare/ghost/god-ray/dust offsets by +4:
/// glares start at `+0x38`, ghosts at `+0x48`, god ray at `+0x90`, dust at `+0x94`
/// (`0x8275de18`).
#[derive(Debug, Clone, Serialize, Default)]
pub struct LensFlare {
    /// `+0x00`: nonzero enables the lens-flare / god-ray / dust pipeline.
    pub enabled: bool,
    /// `+0x04`, `+0x08`: sun/flare pitch and yaw in degrees (converted to radians by `0x827fc620`).
    pub angles_deg: [f32; 2],
    /// `+0x0c..+0x18`: primary glare RGBA (0..255 floats in file, divided by 255).
    pub glare_color: [f32; 4],
    /// `+0x1c..+0x28`: ghost chain RGBA (0..255 floats in file, divided by 255).
    pub ghost_color: [f32; 4],
    /// `+0x2c`: glare alpha flicker amplitude (`r26+0x48`).
    pub glare_flicker: f32,
    /// `+0x30`: ghost alpha flicker amplitude (`r30+0x4c`).
    pub ghost_flicker: f32,
    /// `+0x34..+0x44`: up to 4 primary glare/streak slots.
    pub glares: Vec<FlareGlare>,
    /// `+0x44..+0x8c`: up to 18 ghost chain slots.
    pub ghosts: Vec<FlareGhost>,
    /// `+0x8c`: god-ray radial-blur configuration.
    pub god_ray: GodRay,
    /// `+0x90`: dirty-lens dust overlay configuration.
    pub dust: LensDust,
}

impl LensFlare {
    /// Unit vector toward the flare source in game axes: `(-cos p * sin y, sin p, -cos p * cos y)`.
    pub fn direction(&self) -> [f32; 3] {
        let [p, y] = self.angles_deg.map(|a| a.to_radians());
        [-p.cos() * y.sin(), p.sin(), -p.cos() * y.cos()]
    }
}

/// ACV type-101 `+0x30` screen-filter record.
///
/// `0x8275d4f0` copies `+1..+3`, the `+4..+0x30` floats, and the
/// `+0x1c..+0x20` bytes into the filter singleton at `0x8387c700+0x55c0`
/// through `0x82569e10`. Field meanings are still being traced, so this keeps
/// the raw layout. m0300's first (lightning) and last (scene) events carry the
/// same record: enabled, kind `0x3c`, `params[0]` `0.2`, `params[3]` `-0.13`,
/// `params[5]` `0.06`. m0600's first event is kind `0x50` (`params[0]` `0.5`,
/// `params[3]` `0.1`, `params[4]` `-0.02`, `params[5]` `0.005`, flags
/// `00/28/00/00/00`), while its last event is kind `0xff`, `sub_kind`
/// `0x3c/0x1e`, `params[0]` `0.35`.
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct AcvScreenFilter {
    /// `+0`: nonzero enables the filter.
    pub enabled: bool,
    /// `+1`: raw filter kind.
    pub kind: u8,
    /// `+2/+3`: raw signed sub-kinds.
    pub sub_kind: [i8; 2],
    /// `+4`, `+8`, `+0xc`, `+0x10`, `+0x14`, `+0x18`, `+0x20`, `+0x24`,
    /// `+0x28`, `+0x2c`, `+0x30`.
    pub params: [f32; 11],
    /// `+0x1c..+0x20`: raw bytes (the `+0x20` byte shares a word with `params[6]`).
    pub flags: [u8; 5],
}

/// ACV type-101 `+0x34` depth-of-field record.
///
/// `0x827621ec` copies the 13 floats to scene `+0xf8..+0x120`; the Dof filter
/// object `0x82541fb8` initializes `+0xf8..+0x108` and its execute
/// `0x825422fc` reads `+0xf8..+0x104`. m0600's first five values are
/// `100/1.57/7/6/4`; m0300 is disabled (all zero except `+0x30 = 1`).
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct AcvDof {
    /// `+0..+0x30`: 13 floats copied to the scene record.
    pub params: [f32; 13],
    /// `+0x38..+0x3d`: raw flag bytes.
    pub flags: [u8; 6],
    /// `+0x40/+0x44`: raw trailing words (not read by `0x827621ec`).
    pub tail: [u32; 2],
}

/// The type-101 scene record's sub-blocks (`0x827ffa80`).
#[derive(Debug, Clone, Serialize)]
pub struct Scene {
    /// Bytes 0..3 of the first sub-block, RGB 0-255.
    pub clear: [u8; 3],
    /// The first sub-block's `+4`, the view far distance.
    pub far: f32,
    /// The first sub-block's `+8`, water surface height (`水面の高さ`, stored at runtime `+0x14`
    /// by `0x827ffa80`).
    pub water_height: f32,
    /// `+0x1c`, the sky dome scale (debug label 天球スケール); 0x827ffa80 keeps the default 1
    /// unless it is positive.
    pub sky_scale: f32,
    /// Second sub-block (`data + 8`), tone-map parameters.
    pub tonemap: ToneMap,
    /// Third sub-block (`data + 0xc`), lens-flare, god-ray, and lens-dust parameters.
    pub lens_flare: LensFlare,
    /// ACV only: type-101 `+0x30` screen-filter record (parsed, not rendered yet).
    pub acv_filter: Option<AcvScreenFilter>,
    /// ACV only: type-101 `+0x34` depth-of-field record (parsed, not rendered yet).
    pub acv_dof: Option<AcvDof>,
}

/// Type-400 BGM event data (`0x82800128`: `+0x00` → `r31+0x50`, `+0x04` → `+0x90`, `+0x08` → `+0x94`).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Bgm {
    pub id: u32,
    pub param_a: u32,
    pub param_b: u32,
}

/// `POINT_PARAM_ST` dynamic point light (`kind == 1300`, `shape == 2` Sphere; 360 `0x82be7be0`).
#[derive(Debug, Clone, Serialize)]
pub struct PointLight {
    pub name: String,
    /// World position in game axes (`+0x14..+0x20`).
    pub position: [f32; 3],
    /// Sphere radius (`shape_data + 0x00`).
    pub radius: f32,
    /// Entity `+0x00`: nonzero enables the light.
    pub enabled: bool,
    /// Entity `+0x01`: shadow / extra flag (`light + 0x45`).
    pub flag: u8,
    /// Entity `+0x04`: intensity/scale factor (`light + 0x34`).
    pub scale: f32,
    /// Entity `+0x08..+0x17`: diffuse RGBA (360 converts with `rgb^2 * w` at `0x82d10984` -> `0x82bd2268`).
    pub diffuse: [f32; 4],
    /// Entity `+0x18..+0x27`: specular RGBA (`0x82d10994` -> `0x82bd2268`).
    pub specular: [f32; 4],
    /// Entity `+0x28`: attenuation falloff parameter (`light + 0x30`, `0x82d10874`).
    pub falloff: f32,
}

/// `POINT_PARAM_ST` dynamic spot light (`kind == 1310`, `shape == 3` Cylinder/Cone; 360 `0x82be7d08`).
#[derive(Debug, Clone, Serialize)]
pub struct SpotLight {
    pub name: String,
    /// World position in game axes (`+0x14..+0x20`).
    pub position: [f32; 3],
    /// Euler angles in degrees (`+0x20..+0x2c`; `0x82be7de8`: `yaw = ry + 180`, `pitch = 270 - rx`).
    pub rotation_deg: [f32; 3],
    /// Entity `+0x00`: nonzero enables the light.
    pub enabled: bool,
    /// Entity `+0x01`: shadow flag (`light + 0x45`, checked in `0x82cbc058`).
    pub flag: u8,
    /// Entity `+0x04`: scale factor (`light + 0x34`).
    pub scale: f32,
    /// Entity `+0x08..+0x17`: diffuse RGBA.
    pub diffuse: [f32; 4],
    /// Entity `+0x18..+0x27`: specular RGBA.
    pub specular: [f32; 4],
    /// Entity `+0x28`: attenuation falloff parameter (`light + 0x30`).
    pub falloff: f32,
    /// Entity `+0x2c`: near clip distance (`0x82c03610`).
    pub near: f32,
    /// Entity `+0x30`: far range (`0x82c03610`).
    pub far: f32,
    /// Entity `+0x34`: vertical cone angle in degrees (`0x82be81d0`).
    pub fov_y_deg: f32,
    /// Entity `+0x38`: horizontal cone angle in degrees (`0x82be81d0`).
    pub fov_x_deg: f32,
}

impl SpotLight {
    /// Unit forward beam direction in game axes, matching `0x82be7de8..0x82be7e20`
    /// (`pitch = 270 - rx`, `yaw = ry + 180`, `+Z` rotated by `Ry(yaw) * Rx(pitch)`).
    pub fn direction(&self) -> [f32; 3] {
        let pitch = (270.0 - self.rotation_deg[0]).to_radians();
        let yaw = (self.rotation_deg[1] + 180.0).to_radians();
        // Forward vector: pitch=+90 points straight down (0, -1, 0).
        let cp = pitch.cos();
        [cp * yaw.sin(), -pitch.sin(), cp * yaw.cos()]
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Env {
    pub events: Vec<Event>,
    pub lights: Vec<LightSet>,
    pub scene: Option<Scene>,
    /// ACV only: every type-101 scene event in encounter order. `scene` is the
    /// last of these (last-event-wins); the first is the lightning-filter scene
    /// on m0300/m0600 and carries its own `acv_filter` / `acv_dof`.
    pub acv_scenes: Vec<Scene>,
    pub bgm: Option<Bgm>,
    /// Dynamic point lights from `POINT_PARAM_ST` (`kind == 1300`, `0x82be7be0`).
    pub point_lights: Vec<PointLight>,
    /// Dynamic spot lights from `POINT_PARAM_ST` (`kind == 1310`, `0x82be7d08`).
    pub spot_lights: Vec<SpotLight>,
}

impl Env {
    pub fn light(&self, slot: u32) -> Option<&LightSet> {
        self.lights.iter().find(|l| l.slot == slot)
    }

    /// ACV only: the screen filter from the first type-101 scene event (the
    /// lightning filter scene on m0300/m0600), kept apart from `scene` because
    /// the last event overwrites the core values.
    pub fn acv_first_filter(&self) -> Option<&AcvScreenFilter> {
        self.acv_scenes.first()?.acv_filter.as_ref()
    }
}

fn vec4(r: &Be, at: usize) -> Result<[f32; 4]> {
    Ok([r.f32(at)?, r.f32(at + 4)?, r.f32(at + 8)?, r.f32(at + 12)?])
}

fn light(r: &Be, name: String, d: usize) -> Result<LightSet> {
    Ok(LightSet {
        name,
        slot: r.u8(d + 1)? as u32 * 32 + r.u8(d)? as u32,
        angles: [
            [r.i16(d + 2)?, r.i16(d + 4)?],
            [r.i16(d + 0x28)?, r.i16(d + 0x2a)?],
        ],
        colors: [vec4(r, d + 8)?, vec4(r, d + 0x2c)?],
        colors_b: [vec4(r, d + 0x18)?, vec4(r, d + 0x3c)?],
        sky: vec4(r, d + 0x70)?,
        ground: vec4(r, d + 0x80)?,
        extra: vec4(r, d + 0x90)?,
        fog: Fog {
            color: vec4(r, d + 0xa0)?,
            range: r.f32(d + 0xb0)?,
            start: r.f32(d + 0xb4)?,
            falloff: r.f32(d + 0xbc)?,
            density: r.f32(d + 0xc0)?,
            luma_tint: r.f32(d + 0xc4)?,
            rising: r.u8(d + 0xc8)? != 0,
            height_range: r.f32(d + 0xcc)?,
            height_base: r.f32(d + 0xd0)?,
            height_color: vec4(r, d + 0xd4)?,
            acv: None,
        },
    })
}

/// ACV type-100 payload (`0x827626d8`): one group/id, two i16 angle pairs, six RGBA byte
/// colours and the compact fog block at `+0x30..+0x4d`.
///
/// The runtime writes the fog into its 0xa0-byte slot as `+0/+4` distance start/range (s32
/// converted to f32), `+0x10/+0x40` the two RGB colours, `+0x20/+0x24/+0x50/+0x54` the lower /
/// upper distance and height alphas, and `+0x28/+0x58` the range-sign flags; `0x82b05428`
/// converts the raw byte alphas and colours with 1/255 (`0x82017d6c`) into the final slot.
/// Distance start/range are view-space z, not horizontal distance.
fn acv_light(r: &Be, name: String, d: usize) -> Result<LightSet> {
    let color = |at: usize| -> Result<[f32; 4]> {
        Ok([
            r.u8(at)? as f32 / 255.0,
            r.u8(at + 1)? as f32 / 255.0,
            r.u8(at + 2)? as f32 / 255.0,
            r.u8(at + 3)? as f32 / 255.0,
        ])
    };
    let unit = |v: u8| v as f32 / 255.0;
    let distance_max = r.u8(d + 0x3c)?;
    let height_max = r.u8(d + 0x4c)?;
    let distance_inverted = r.u8(d + 0x3d)? != 0;
    let height_inverted = r.u8(d + 0x4d)? != 0;
    Ok(LightSet {
        name,
        slot: r.u8(d + 1)? as u32 * 32 + r.u8(d)? as u32,
        angles: [
            [r.i16(d + 2)?, r.i16(d + 4)?],
            [r.i16(d + 0xe)?, r.i16(d + 0x10)?],
        ],
        colors: [color(d + 6)?, color(d + 0x12)?],
        colors_b: [color(d + 0xa)?, color(d + 0x16)?],
        sky: color(d + 0x26)?,
        ground: color(d + 0x2a)?,
        extra: [1.0; 4],
        fog: Fog {
            color: [
                unit(r.u8(d + 0x38)?),
                unit(r.u8(d + 0x39)?),
                unit(r.u8(d + 0x3a)?),
                unit(distance_max),
            ],
            range: r.i32(d + 0x34)? as f32,
            start: r.i32(d + 0x30)? as f32,
            falloff: 1.0,
            density: 1.0,
            luma_tint: 0.0,
            rising: !height_inverted,
            height_range: r.i32(d + 0x44)? as f32,
            height_base: r.i32(d + 0x40)? as f32,
            height_color: [
                unit(r.u8(d + 0x48)?),
                unit(r.u8(d + 0x49)?),
                unit(r.u8(d + 0x4a)?),
                unit(height_max),
            ],
            acv: Some(AcvFog {
                distance_min: unit(r.u8(d + 0x3b)?),
                height_min: unit(r.u8(d + 0x4b)?),
                distance_inverted,
                height_inverted,
            }),
        },
    })
}

/// ACV type-101 core scene (`0x827621ec`): first sub-block at `data + u32(data)`.
///
/// Unlike VD, ACV keeps the core values in the first sub-block but at different offsets:
/// `+4` draw distance, `+8` water height, `+0x1c` sky scale (only used when `> 1`). The
/// lens-flare sub-block is at `data + u32(data + 0x14)`; the screen filter (`+0x30`) and
/// DOF (`+0x34`) records are parsed but not rendered yet. The `+0x1c` atmosphere record
/// is documented in `sheets/map_env.csv` but not parsed; it is not the type-100 fog.
fn acv_screen_filter(r: &Be, d: usize) -> Result<Option<AcvScreenFilter>> {
    let rel = r.u32(d + 0x30)? as usize;
    if rel == 0 {
        return Ok(None);
    }
    let s = d + rel;
    if s + 0x34 > r.len() {
        return Ok(None);
    }
    Ok(Some(AcvScreenFilter {
        enabled: r.u8(s)? != 0,
        kind: r.u8(s + 1)?,
        sub_kind: [r.i8(s + 2)?, r.i8(s + 3)?],
        params: [
            r.f32(s + 4)?,
            r.f32(s + 8)?,
            r.f32(s + 0xc)?,
            r.f32(s + 0x10)?,
            r.f32(s + 0x14)?,
            r.f32(s + 0x18)?,
            r.f32(s + 0x20)?,
            r.f32(s + 0x24)?,
            r.f32(s + 0x28)?,
            r.f32(s + 0x2c)?,
            r.f32(s + 0x30)?,
        ],
        flags: [
            r.u8(s + 0x1c)?,
            r.u8(s + 0x1d)?,
            r.u8(s + 0x1e)?,
            r.u8(s + 0x1f)?,
            r.u8(s + 0x20)?,
        ],
    }))
}

fn acv_dof(r: &Be, d: usize) -> Result<Option<AcvDof>> {
    let rel = r.u32(d + 0x34)? as usize;
    if rel == 0 {
        return Ok(None);
    }
    let s = d + rel;
    if s + 0x48 > r.len() {
        return Ok(None);
    }
    let mut params = [0.0; 13];
    for (i, p) in params.iter_mut().enumerate() {
        *p = r.f32(s + i * 4)?;
    }
    let mut flags = [0; 6];
    for (i, f) in flags.iter_mut().enumerate() {
        *f = r.u8(s + 0x38 + i)?;
    }
    Ok(Some(AcvDof {
        params,
        flags,
        tail: [r.u32(s + 0x40)?, r.u32(s + 0x44)?],
    }))
}

fn acv_scene(r: &Be, d: usize) -> Result<Scene> {
    let sub = d + r.u32(d)? as usize;
    let flare_rel = r.u32(d + 0x14)? as usize;
    let lens_flare = if flare_rel > 0 && d + flare_rel < r.len() {
        acv_lens_flare(r, d + flare_rel, r.len())?
    } else {
        LensFlare::default()
    };
    Ok(Scene {
        clear: [r.u8(sub)?, r.u8(sub + 1)?, r.u8(sub + 2)?],
        far: r.f32(sub + 4)?,
        water_height: r.f32(sub + 8)?,
        sky_scale: Some(r.f32(sub + 0x1c)?)
            .filter(|&s| s > 1.0)
            .unwrap_or(1.0),
        tonemap: ToneMap::default(),
        lens_flare,
        acv_filter: acv_screen_filter(r, d)?,
        acv_dof: acv_dof(r, d)?,
    })
}

#[derive(Clone, Copy)]
struct LensFlareOffsets {
    glares: usize,
    ghosts: usize,
    god_ray: usize,
    dust: usize,
}

const VD_LENS_FLARE: LensFlareOffsets = LensFlareOffsets {
    glares: 0x34,
    ghosts: 0x44,
    god_ray: 0x8c,
    dust: 0x90,
};

const ACV_LENS_FLARE: LensFlareOffsets = LensFlareOffsets {
    glares: 0x38,
    ghosts: 0x48,
    god_ray: 0x90,
    dust: 0x94,
};

fn lens_flare(r: &Be, s3: usize, len: usize) -> Result<LensFlare> {
    lens_flare_at(r, s3, len, VD_LENS_FLARE)
}

fn acv_lens_flare(r: &Be, s3: usize, len: usize) -> Result<LensFlare> {
    lens_flare_at(r, s3, len, ACV_LENS_FLARE)
}

fn lens_flare_at(
    r: &Be,
    s3: usize,
    len: usize,
    offsets: LensFlareOffsets,
) -> Result<LensFlare> {
    if s3 + offsets.dust + 4 > len {
        return Ok(LensFlare::default());
    }
    let enabled = r.u32(s3)? != 0;
    if !enabled {
        return Ok(LensFlare::default());
    }
    const INV255: f32 = 1.0 / 255.0;
    let angles_deg = [r.f32(s3 + 4)?, r.f32(s3 + 8)?];
    let glare_color = [
        r.f32(s3 + 0x0c)? * INV255,
        r.f32(s3 + 0x10)? * INV255,
        r.f32(s3 + 0x14)? * INV255,
        r.f32(s3 + 0x18)? * INV255,
    ];
    let ghost_color = [
        r.f32(s3 + 0x1c)? * INV255,
        r.f32(s3 + 0x20)? * INV255,
        r.f32(s3 + 0x24)? * INV255,
        r.f32(s3 + 0x28)? * INV255,
    ];
    let glare_flicker = r.f32(s3 + 0x2c)?;
    let ghost_flicker = r.f32(s3 + 0x30)?;

    let mut glares = Vec::new();
    for i in 0..4 {
        let rel = r.u32(s3 + offsets.glares + i * 4)? as usize;
        if rel > 0 && s3 + rel + 0x10 <= len {
            let p = s3 + rel;
            let tex_id = r.i32(p)?;
            if tex_id >= 0 {
                glares.push(FlareGlare {
                    tex_id,
                    alpha: r.u8(p + 4)? as f32 * INV255,
                    scale: r.f32(p + 8)?,
                    scale_by_vis: r.u8(p + 0x0c)? != 0,
                    rotate: r.u8(p + 0x0d)? != 0,
                    mode: r.u8(p + 0x0e)?,
                });
            }
        }
    }

    let mut ghosts = Vec::new();
    for i in 0..18 {
        let rel = r.u32(s3 + offsets.ghosts + i * 4)? as usize;
        if rel > 0 && s3 + rel + 0x12 <= len {
            let p = s3 + rel;
            let tex_id = r.i32(p)?;
            if tex_id >= 0 {
                ghosts.push(FlareGhost {
                    tex_id,
                    alpha: r.u8(p + 4)? as f32 * INV255,
                    scale: r.f32(p + 8)?,
                    axis_pos: r.f32(p + 0x0c)?,
                    rotate: r.u8(p + 0x10)? != 0,
                    alt_scale: r.u8(p + 0x11)? != 0,
                });
            }
        }
    }

    let gr_rel = r.u32(s3 + offsets.god_ray)? as usize;
    let god_ray = if gr_rel > 0 && s3 + gr_rel + 0x2c <= len {
        let p = s3 + gr_rel;
        GodRay {
            tex_id: r.u32(p)? as i16,
            emitter_scale: r.f32(p + 4)?,
            emitter_depth_bias: r.f32(p + 8)?,
            emitter_color: [r.u8(p + 0x0c)?, r.u8(p + 0x0d)?, r.u8(p + 0x0e)?, r.u8(p + 0x0f)?],
            blur_params: [
                r.f32(p + 0x10)?,
                r.f32(p + 0x14)?,
                r.f32(p + 0x18)?,
                r.f32(p + 0x1c)?,
            ],
            merge_intensity: r.f32(p + 0x20)?,
            sample_mode: r.u16(p + 0x24)? as u8,
            merge_mode: r.u16(p + 0x26)? as u8,
            merge_color: [r.u8(p + 0x28)?, r.u8(p + 0x29)?, r.u8(p + 0x2a)?, r.u8(p + 0x2b)?],
        }
    } else {
        GodRay::default()
    };

    let dust_rel = r.u32(s3 + offsets.dust)? as usize;
    let dust = if dust_rel > 0 && s3 + dust_rel + 0x14 <= len {
        let p = s3 + dust_rel;
        LensDust {
            pos: [r.f32(p)?, r.f32(p + 4)?],
            size: [r.f32(p + 8)?, r.f32(p + 0x0c)?],
            color: [r.u8(p + 0x10)?, r.u8(p + 0x11)?, r.u8(p + 0x12)?, r.u8(p + 0x13)?],
        }
    } else {
        LensDust::default()
    };

    Ok(LensFlare {
        enabled,
        angles_deg,
        glare_color,
        ghost_color,
        glare_flicker,
        ghost_flicker,
        glares,
        ghosts,
        god_ray,
        dust,
    })
}

pub fn read(data: &[u8]) -> Result<Env> {
    let r = Be(data);
    let magic = r.u32(0)?;
    ensure!(
        matches!(magic, msb::MAGIC_ENV | msb::MAGIC),
        "not an env MSB"
    );
    // ACV's type-100 lights (with their compact fog) and type-101 core scene + lens flare +
    // screen filter + DOF are packed layouts decoded by `acv_light` / `acv_scene` /
    // `acv_lens_flare`; every type-101 scene event is kept in `acv_scenes`, while `scene`
    // is the last one. The type-101 atmosphere record stays unparsed.
    let acv = magic == msb::MAGIC;
    let mut env = Env {
        events: Vec::new(),
        lights: Vec::new(),
        scene: None,
        acv_scenes: Vec::new(),
        bgm: None,
        point_lights: Vec::new(),
        spot_lights: Vec::new(),
    };
    for off in msb::section(data, "EVENT_PARAM_ST")? {
        let name_rel = r.u32(off)? as usize;
        ensure!(
            name_rel < 0x400,
            "env event at {off:#x} name offset {name_rel:#x}"
        );
        let e = Event {
            name: r.cstr_sjis(off + name_rel)?,
            id: r.i32(off + 4)?,
            kind: r.u32(off + 8)?,
            index: r.u32(off + 0xc)?,
            data: off + r.u32(off + 0x10)? as usize,
        };
        match e.kind {
            EVENT_LIGHT if acv => env.lights.push(acv_light(&r, e.name.clone(), e.data)?),
            EVENT_LIGHT => env.lights.push(light(&r, e.name.clone(), e.data)?),
            EVENT_SCENE if acv => {
                let scene = acv_scene(&r, e.data)?;
                env.scene = Some(scene.clone());
                env.acv_scenes.push(scene);
            }
            EVENT_SCENE if env.scene.is_none() => {
                let sub = e.data + r.u32(e.data)? as usize;
                let sub2_rel = r.u32(e.data + 8)? as usize;
                let tonemap = if sub2_rel > 0 && e.data + sub2_rel + 0x14 <= data.len() {
                    let s2 = e.data + sub2_rel;
                    let bloom = if s2 + 0x3c <= data.len() {
                        Bloom {
                            iters: r.u8(s2 + 0x19)?,
                            enabled: r.u8(s2 + 0x1a)? != 0,
                            threshold: r.f32(s2 + 0x1c)?,
                            offset: r.f32(s2 + 0x20)?,
                            blur_scale: [r.f32(s2 + 0x24)?, r.f32(s2 + 0x28)?],
                            color: vec4(&r, s2 + 0x2c)?,
                        }
                    } else {
                        Bloom::default()
                    };
                    ToneMap {
                        mode: r.u32(s2)?,
                        lum: r.f32(s2 + 4)?,
                        mul: r.f32(s2 + 8)?,
                        key: r.f32(s2 + 0x0c)?,
                        white: r.f32(s2 + 0x10)?,
                        bloom,
                    }
                } else {
                    ToneMap::default()
                };
                let sub3_rel = r.u32(e.data + 0x0c)? as usize;
                let lens_flare = if sub3_rel > 0 {
                    lens_flare(&r, e.data + sub3_rel, data.len())?
                } else {
                    LensFlare::default()
                };
                env.scene = Some(Scene {
                    clear: [r.u8(sub)?, r.u8(sub + 1)?, r.u8(sub + 2)?],
                    far: r.f32(sub + 4)?,
                    water_height: r.f32(sub + 8)?,
                    sky_scale: Some(r.f32(sub + 0x1c)?).filter(|&s| s > 0.0).unwrap_or(1.0),
                    tonemap,
                    lens_flare,
                    acv_filter: None,
                    acv_dof: None,
                });
            }
            EVENT_BGM if env.bgm.is_none() && e.data + 12 <= data.len() => {
                env.bgm = Some(Bgm {
                    id: r.u32(e.data)?,
                    param_a: r.u32(e.data + 4)?,
                    param_b: r.u32(e.data + 8)?,
                });
            }
            _ => {}
        }
        env.events.push(e);
    }
    if let Ok(points) = msb::section(data, "POINT_PARAM_ST") {
        for off in points {
            let kind = r.u32(off + 8)?;
            if kind != POINT_LIGHT && kind != SPOT_LIGHT {
                continue;
            }
            let shape = r.u32(off + 0x10)?;
            let name_rel = r.u32(off)? as usize;
            let name = r.cstr_sjis(off + name_rel)?;
            let position = [r.f32(off + 0x14)?, r.f32(off + 0x18)?, r.f32(off + 0x1c)?];
            let rotation_deg = [r.f32(off + 0x20)?, r.f32(off + 0x24)?, r.f32(off + 0x28)?];
            let s_rel = r.u32(off + 0x38)? as usize;
            let e_rel = r.u32(off + 0x44)? as usize;
            if s_rel == 0 || e_rel == 0 {
                continue;
            }
            let s_off = off + s_rel;
            let e_off = off + e_rel;
            if kind == POINT_LIGHT && shape == 2 && e_off + 0x2c <= data.len() {
                env.point_lights.push(PointLight {
                    name,
                    position,
                    radius: r.f32(s_off)?,
                    enabled: r.u8(e_off)? != 0,
                    flag: r.u8(e_off + 1)?,
                    scale: r.f32(e_off + 4)?,
                    diffuse: vec4(&r, e_off + 8)?,
                    specular: vec4(&r, e_off + 0x18)?,
                    falloff: r.f32(e_off + 0x28)?,
                });
            } else if kind == SPOT_LIGHT && shape == 3 && e_off + 0x3c <= data.len() {
                env.spot_lights.push(SpotLight {
                    name,
                    position,
                    rotation_deg,
                    enabled: r.u8(e_off)? != 0,
                    flag: r.u8(e_off + 1)?,
                    scale: r.f32(e_off + 4)?,
                    diffuse: vec4(&r, e_off + 8)?,
                    specular: vec4(&r, e_off + 0x18)?,
                    falloff: r.f32(e_off + 0x28)?,
                    near: r.f32(e_off + 0x2c)?,
                    far: r.f32(e_off + 0x30)?,
                    fov_y_deg: r.f32(e_off + 0x34)?,
                    fov_x_deg: r.f32(e_off + 0x38)?,
                });
            }
        }
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// Values the 360 uploaded for m4000's map set (`private/xenia/envlook_mission2.jsonl`).
    #[test]
    fn m4000_env_from_disc() {
        let Some(disc) = crate::vfs::test_disc() else {
            return;
        };
        let env =
            read(&crate::vfs::open(&disc, "model/map/ch_env/m4000_env.msb").unwrap()).unwrap();
        assert_eq!(env.events.len(), 21);
        assert_eq!(env.events.iter().filter(|e| e.kind == EVENT_BGM).count(), 1);
        let slots: Vec<u32> = env.lights.iter().map(|l| l.slot).collect();
        assert_eq!(&slots[..6], &[0, 1, 2, 3, 4, 6]);
        assert!(slots.contains(&19));

        let map = env.light(1).unwrap();
        let d = map.direction(0);
        assert!(
            near(d[0], -0.4777) && near(d[1], 0.6691) && near(d[2], 0.5693),
            "{d:?}"
        );
        assert_eq!(map.angles, [[42, 140], [-6, 30]]);
        assert_eq!(map.colors[0], [1.5, 1.5, 1.4, 1.6]);
        assert_eq!(map.fog.color, [0.8, 0.8, 0.8, 0.6]);
        assert_eq!(
            (map.fog.start, map.fog.range, map.fog.falloff),
            (30.0, 375.0, 10.0)
        );
        assert!(map.fog.rising);
        assert_eq!(map.fog.height_range, 0.0);

        let mountain = env.light(2).unwrap();
        assert_eq!(
            (mountain.fog.height_range, mountain.fog.height_base),
            (20.0, 50.0)
        );
        assert!(near(mountain.fog.height_color[3], 0.14));

        let scene = env.scene.as_ref().unwrap();
        assert_eq!(scene.clear, [0x25, 0x32, 0x31]);
        assert_eq!(scene.far, 15000.0);
        assert_eq!(scene.water_height, 3.0);
        assert_eq!(scene.sky_scale, 100.0);
        assert_eq!(scene.tonemap.mode, 1);
        assert!(near(scene.tonemap.lum, 0.5));
        assert!(near(scene.tonemap.mul, 1.1));
        assert!(near(scene.tonemap.key, 0.6));
        assert!(near(scene.tonemap.white, 10.0));
        assert!(scene.tonemap.bloom.enabled);
        assert_eq!(scene.tonemap.bloom.iters, 3);
        assert!(near(scene.tonemap.bloom.threshold, 0.9));
        assert!(near(scene.tonemap.bloom.offset, 0.8));
        assert!(
            near(scene.tonemap.bloom.blur_scale[0], 1.0)
                && near(scene.tonemap.bloom.blur_scale[1], 1.0)
        );
        assert!(scene.tonemap.bloom.color.iter().all(|&c| near(c, 1.0)));

        let lf = &scene.lens_flare;
        assert!(lf.enabled);
        assert!(near(lf.angles_deg[0], 43.0) && near(lf.angles_deg[1], -160.0));
        assert_eq!(lf.glares.len(), 4);
        assert_eq!(lf.ghosts.len(), 6);
        assert_eq!(lf.god_ray.tex_id, -1);
        assert!(near(lf.god_ray.blur_params[0], 0.3));
        assert!(near(lf.god_ray.blur_params[1], 0.98));
        assert!(near(lf.god_ray.blur_params[2], 0.98));
        assert!(near(lf.god_ray.blur_params[3], 1.0));
        assert!(near(lf.dust.pos[0], -80.0) && near(lf.dust.pos[1], 98.0));
        assert!(near(lf.dust.size[0], 752.0) && near(lf.dust.size[1], 672.0));
        assert_eq!(lf.dust.color, [255, 255, 155, 255]);
    }

    #[test]
    fn acv_scene_core_layout() {
        let mut data = vec![0u8; 0x40];
        data[..4].copy_from_slice(&0x20u32.to_be_bytes());
        data[0x20..0x23].copy_from_slice(&[0x25, 0x32, 0x31]);
        data[0x24..0x28].copy_from_slice(&10000.0f32.to_be_bytes());
        data[0x28..0x2c].copy_from_slice(&(-1000.0f32).to_be_bytes());
        data[0x3c..0x40].copy_from_slice(&100.0f32.to_be_bytes());
        let scene = acv_scene(&Be(&data), 0).unwrap();
        assert_eq!(scene.clear, [0x25, 0x32, 0x31]);
        assert!(near(scene.far, 10000.0));
        assert!(near(scene.water_height, -1000.0));
        assert!(near(scene.sky_scale, 100.0));
    }

    #[test]
    fn acv_lens_flare_layout() {
        let mut data = vec![0u8; 0x500];
        data[..4].copy_from_slice(&0x20u32.to_be_bytes());
        data[0x14..0x18].copy_from_slice(&0x80u32.to_be_bytes());
        let f = 0x80;
        data[f..f + 4].copy_from_slice(&1u32.to_be_bytes());
        data[f + 4..f + 8].copy_from_slice(&17.0f32.to_be_bytes());
        data[f + 8..f + 0xc].copy_from_slice(&29.0f32.to_be_bytes());
        for (i, v) in [
            255.0f32, 165.0, 115.0, 270.0, 154.0, 225.0, 255.0, 140.0,
        ]
        .into_iter()
        .enumerate()
        {
            data[f + 0x0c + i * 4..f + 0x10 + i * 4].copy_from_slice(&v.to_be_bytes());
        }
        data[f + 0x2c..f + 0x30].copy_from_slice(&0.05f32.to_be_bytes());
        data[f + 0x30..f + 0x34].copy_from_slice(&0.05f32.to_be_bytes());
        data[f + 0x34..f + 0x38].copy_from_slice(&4u32.to_be_bytes());
        for (i, rel) in [0x200u32, 0x230, 0x260, 0x290].into_iter().enumerate() {
            data[f + 0x38 + i * 4..f + 0x3c + i * 4].copy_from_slice(&rel.to_be_bytes());
        }
        data[f + 0x48..f + 0x4c].copy_from_slice(&0x300u32.to_be_bytes());
        data[f + 0x90..f + 0x94].copy_from_slice(&0x350u32.to_be_bytes());
        data[f + 0x94..f + 0x98].copy_from_slice(&0x3a0u32.to_be_bytes());

        let g = f + 0x200;
        data[g..g + 4].copy_from_slice(&0i32.to_be_bytes());
        data[g + 4] = 136;
        data[g + 8..g + 0xc].copy_from_slice(&3.3f32.to_be_bytes());
        let g = f + 0x230;
        data[g..g + 4].copy_from_slice(&1i32.to_be_bytes());
        data[g + 4] = 255;
        data[g + 8..g + 0xc].copy_from_slice(&0.6f32.to_be_bytes());
        data[g + 0x0c] = 1;
        data[g + 0x0d] = 1;
        let g = f + 0x260;
        data[g..g + 4].copy_from_slice(&10i32.to_be_bytes());
        data[g + 4] = 48;
        data[g + 8..g + 0xc].copy_from_slice(&6.8f32.to_be_bytes());

        let h = f + 0x300;
        data[h..h + 4].copy_from_slice(&8i32.to_be_bytes());
        data[h + 4] = 255;
        data[h + 8..h + 0xc].copy_from_slice(&1.0f32.to_be_bytes());
        data[h + 0x10] = 0;
        data[h + 0x11] = 1;

        let gr = f + 0x350;
        data[gr..gr + 4].copy_from_slice(&0xffffu32.to_be_bytes());
        data[gr + 4..gr + 8].copy_from_slice(&1.0f32.to_be_bytes());
        data[gr + 0x0c..gr + 0x10].copy_from_slice(&[10, 20, 30, 40]);
        for (i, v) in [0.3f32, 0.98, 0.98, 1.0].into_iter().enumerate() {
            data[gr + 0x10 + i * 4..gr + 0x14 + i * 4].copy_from_slice(&v.to_be_bytes());
        }
        data[gr + 0x20..gr + 0x24].copy_from_slice(&1.0f32.to_be_bytes());
        data[gr + 0x24..gr + 0x26].copy_from_slice(&12u16.to_be_bytes());
        data[gr + 0x26..gr + 0x28].copy_from_slice(&0u16.to_be_bytes());
        data[gr + 0x28..gr + 0x2c].copy_from_slice(&[1, 2, 3, 4]);

        let d = f + 0x3a0;
        data[d + 4..d + 8].copy_from_slice(&208.0f32.to_be_bytes());
        data[d + 8..d + 0xc].copy_from_slice(&512.0f32.to_be_bytes());
        data[d + 0x0c..d + 0x10].copy_from_slice(&512.0f32.to_be_bytes());
        data[d + 0x10..d + 0x14].copy_from_slice(&[255, 255, 255, 255]);

        let scene = acv_scene(&Be(&data), 0).unwrap();
        let lf = &scene.lens_flare;
        assert!(lf.enabled);
        assert!(near(lf.angles_deg[0], 17.0) && near(lf.angles_deg[1], 29.0));
        assert!(near(lf.glare_color[3], 270.0 / 255.0));
        assert!(near(lf.ghost_color[0], 154.0 / 255.0));
        assert_eq!(lf.glares.len(), 4);
        assert_eq!(lf.glares[1].tex_id, 1);
        assert!(near(lf.glares[1].alpha, 1.0));
        assert!(lf.glares[1].scale_by_vis && lf.glares[1].rotate);
        assert_eq!(lf.ghosts.len(), 1);
        assert_eq!(lf.ghosts[0].tex_id, 8);
        assert!(!lf.ghosts[0].rotate && lf.ghosts[0].alt_scale);
        assert_eq!(lf.god_ray.tex_id, -1);
        assert_eq!(lf.god_ray.emitter_color, [10, 20, 30, 40]);
        assert!(near(lf.god_ray.blur_params[0], 0.3));
        assert_eq!(lf.god_ray.sample_mode, 12);
        assert_eq!(lf.god_ray.merge_color, [1, 2, 3, 4]);
        assert!(near(lf.dust.pos[1], 208.0));
        assert!(near(lf.dust.size[0], 512.0));
        assert_eq!(lf.dust.color, [255, 255, 255, 255]);
    }

    #[test]
    fn acv_filter_dof_layout() {
        let mut data = vec![0u8; 0x200];
        data[..4].copy_from_slice(&0x20u32.to_be_bytes());
        data[0x30..0x34].copy_from_slice(&0x100u32.to_be_bytes());
        data[0x34..0x38].copy_from_slice(&0x140u32.to_be_bytes());

        let f = 0x100;
        data[f] = 1;
        data[f + 1] = 0x3c;
        data[f + 2] = 0xff;
        data[f + 3] = 0xfe;
        for (i, v) in [0.2f32, 0.0, 0.0, -0.13, 0.0, 0.06, 0.0, 0.0, 0.0, 0.0, 0.0]
            .into_iter()
            .enumerate()
        {
            data[f + 4 + i * 4..f + 8 + i * 4].copy_from_slice(&v.to_be_bytes());
        }
        data[f + 0x1c..f + 0x20].copy_from_slice(&[5, 6, 7, 8]);

        let d = 0x140;
        for (i, v) in [
            100.0f32, 1.57, 7.0, 6.0, 4.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]
        .into_iter()
        .enumerate()
        {
            data[d + i * 4..d + 4 + i * 4].copy_from_slice(&v.to_be_bytes());
        }
        data[d + 0x38..d + 0x3e].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        data[d + 0x40..d + 0x44].copy_from_slice(&0x14u32.to_be_bytes());
        data[d + 0x44..d + 0x48].copy_from_slice(&0x66u32.to_be_bytes());

        let scene = acv_scene(&Be(&data), 0).unwrap();
        let filter = scene.acv_filter.expect("ACV filter");
        assert!(filter.enabled);
        assert_eq!(filter.kind, 0x3c);
        assert_eq!(filter.sub_kind, [-1, -2]);
        assert!(near(filter.params[0], 0.2));
        assert!(near(filter.params[3], -0.13));
        assert!(near(filter.params[5], 0.06));
        assert_eq!(filter.flags, [5, 6, 7, 8, 0]);

        let dof = scene.acv_dof.expect("ACV DOF");
        assert!(near(dof.params[0], 100.0));
        assert!(near(dof.params[1], 1.57));
        assert!(near(dof.params[4], 4.0));
        assert!(near(dof.params[12], 1.0));
        assert_eq!(dof.flags, [1, 2, 3, 4, 5, 6]);
        assert_eq!(dof.tail, [0x14, 0x66]);
    }

    #[test]
    fn acv_light_fog_layout() {
        let mut data = vec![0u8; 0x50];
        data[0] = 1; // slot 1 = ID01 map basic light
        data[0x30..0x34].copy_from_slice(&(-20i32).to_be_bytes());
        data[0x34..0x38].copy_from_slice(&230i32.to_be_bytes());
        data[0x38..0x3b].copy_from_slice(&[30, 120, 80]);
        data[0x3b] = 0;
        data[0x3c] = 90;
        data[0x3d] = 0;
        data[0x40..0x44].copy_from_slice(&(-66i32).to_be_bytes());
        data[0x44..0x48].copy_from_slice(&106i32.to_be_bytes());
        data[0x48..0x4b].copy_from_slice(&[100, 219, 225]);
        data[0x4b] = 0;
        data[0x4c] = 135;
        data[0x4d] = 1;

        let light = acv_light(&Be(&data), "test".into(), 0).unwrap();
        assert_eq!(light.slot, 1);
        let fog = light.fog;
        assert!(near(fog.start, -20.0));
        assert!(near(fog.range, 230.0));
        assert!(near(fog.height_base, -66.0));
        assert!(near(fog.height_range, 106.0));
        assert!(near(fog.color[0], 30.0 / 255.0));
        assert!(near(fog.color[1], 120.0 / 255.0));
        assert!(near(fog.color[2], 80.0 / 255.0));
        assert!(near(fog.color[3], 90.0 / 255.0));
        assert!(near(fog.height_color[0], 100.0 / 255.0));
        assert!(near(fog.height_color[1], 219.0 / 255.0));
        assert!(near(fog.height_color[2], 225.0 / 255.0));
        assert!(near(fog.height_color[3], 135.0 / 255.0));
        assert!(!fog.rising);
        let acv = fog.acv.expect("ACV fog extras");
        assert!(near(acv.distance_min, 0.0));
        assert!(near(acv.height_min, 0.0));
        assert!(!acv.distance_inverted);
        assert!(acv.height_inverted);
    }

    #[test]
    fn acv_env_from_mission_binder() {
        let Some(disc) = crate::vfs::test_disc_v() else {
            return;
        };
        let binder = disc.asset("bind/mission/mission_1200.bnd").unwrap();
        let b = crate::bnd3::read(&binder).unwrap();
        let entry = b
            .entries
            .iter()
            .find(|e| {
                e.name
                    .as_deref()
                    .is_some_and(|n| n.ends_with("m0300_env.msb"))
            })
            .expect("m0300_env.msb");
        let data = crate::vfs::undcx(entry.contents(&binder).unwrap().into_owned()).unwrap();
        assert_eq!(
            u32::from_be_bytes(data[..4].try_into().unwrap()),
            msb::MAGIC
        );
        let env = read(&data).unwrap();
        assert_eq!(env.events.len(), 50);
        assert_eq!(
            env.events.iter().filter(|e| e.kind == EVENT_LIGHT).count(),
            44
        );
        assert_eq!(
            env.events.iter().filter(|e| e.kind == EVENT_SCENE).count(),
            2
        );
        assert_eq!(env.events.iter().filter(|e| e.kind == EVENT_BGM).count(), 1);
        let bgm = env.bgm.unwrap();
        assert_eq!((bgm.id, bgm.param_a, bgm.param_b), (103, 1001, 0));
        assert_eq!(env.lights.len(), 44);
        let light = env.light(0).unwrap();
        assert_eq!(light.angles, [[15, 5], [30, 20]]);
        assert!(near(light.colors[0][1], 196.0 / 255.0));
        assert!(near(light.colors_b[0][1], 245.0 / 255.0));
        assert!(near(light.sky[0], 128.0 / 255.0));
        assert!(near(light.ground[3], 45.0 / 255.0));
        assert!(env.lights.iter().all(|l| l.fog.acv.is_some()));

        let map = env.light(1).expect("ID01 map basic light");
        assert!(near(map.fog.start, -750.0));
        assert!(near(map.fog.range, 820.0));
        assert!(near(map.fog.height_base, 0.0));
        assert!(near(map.fog.height_range, 24.0));
        assert!(near(map.fog.color[0], 90.0 / 255.0));
        assert!(near(map.fog.color[1], 120.0 / 255.0));
        assert!(near(map.fog.color[2], 110.0 / 255.0));
        assert!(near(map.fog.color[3], 90.0 / 255.0));
        assert!(near(map.fog.height_color[0], 126.0 / 255.0));
        assert!(near(map.fog.height_color[1], 215.0 / 255.0));
        assert!(near(map.fog.height_color[2], 175.0 / 255.0));
        assert!(near(map.fog.height_color[3], 100.0 / 255.0));
        assert!(!map.fog.rising);
        let acv_fog = map.fog.acv.expect("ACV fog extras");
        assert!(near(acv_fog.distance_min, 0.0));
        assert!(near(acv_fog.height_min, 0.0));
        assert!(!acv_fog.distance_inverted);
        assert!(acv_fog.height_inverted);

        let scene = env.scene.as_ref().expect("ACV core scene");
        assert_eq!(scene.clear, [0x25, 0x32, 0x31]);
        assert!(near(scene.far, 10000.0));
        assert!(near(scene.water_height, -1000.0));
        assert!(near(scene.sky_scale, 100.0));
        let lf = &scene.lens_flare;
        assert!(lf.enabled);
        assert!(near(lf.angles_deg[0], 17.0) && near(lf.angles_deg[1], 29.0));
        assert!(near(lf.glare_color[1], 165.0 / 255.0));
        assert!(near(lf.glare_color[3], 270.0 / 255.0));
        assert!(near(lf.ghost_color[3], 140.0 / 255.0));
        assert_eq!(lf.glares.len(), 4);
        assert_eq!(lf.glares[0].tex_id, 0);
        assert!(near(lf.glares[0].alpha, 136.0 / 255.0));
        assert!(near(lf.glares[0].scale, 3.3));
        assert_eq!(lf.glares[3].tex_id, 10);
        assert!(lf.glares[3].scale_by_vis && lf.glares[3].rotate);
        assert_eq!(lf.ghosts.len(), 5);
        assert_eq!(lf.ghosts[0].tex_id, 8);
        assert!(near(lf.ghosts[0].alpha, 1.0));
        assert!(near(lf.ghosts[0].axis_pos, 0.0));
        assert!(!lf.ghosts[0].rotate && lf.ghosts[0].alt_scale);
        assert_eq!(lf.god_ray.tex_id, -1);
        assert!(near(lf.god_ray.emitter_scale, 1.0));
        assert!(near(lf.god_ray.blur_params[0], 0.3));
        assert!(near(lf.god_ray.blur_params[1], 0.98));
        assert!(near(lf.god_ray.blur_params[2], 0.98));
        assert!(near(lf.god_ray.blur_params[3], 1.0));
        assert!(near(lf.dust.pos[0], 0.0) && near(lf.dust.pos[1], 208.0));
        assert!(near(lf.dust.size[0], 512.0) && near(lf.dust.size[1], 512.0));
        assert_eq!(lf.dust.color, [255, 255, 255, 255]);

        let filter = scene.acv_filter.expect("ACV screen filter");
        assert!(filter.enabled);
        assert_eq!(filter.kind, 0x3c);
        assert_eq!(filter.sub_kind, [0, 0]);
        assert!(near(filter.params[0], 0.2));
        assert!(near(filter.params[3], -0.13));
        assert!(near(filter.params[5], 0.06));
        let dof = scene.acv_dof.expect("ACV DOF");
        assert!(near(dof.params[0], 0.0));
        assert!(near(dof.params[12], 1.0));
        assert_eq!(dof.flags, [0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn acv_two_scene_events_keep_first_filter() {
        let Some(disc) = crate::vfs::test_disc_v() else {
            return;
        };
        let env_from_binder = |binder_path: &str, msb: &str| {
            let binder = disc.asset(binder_path).unwrap();
            let b = crate::bnd3::read(&binder).unwrap();
            let entry = b
                .entries
                .iter()
                .find(|e| e.name.as_deref().is_some_and(|n| n.ends_with(msb)))
                .unwrap_or_else(|| panic!("{msb} in {binder_path}"));
            let data = crate::vfs::undcx(entry.contents(&binder).unwrap().into_owned()).unwrap();
            read(&data).unwrap()
        };
        let env = env_from_binder("bind/mission/mission_1200.bnd", "m0300_env.msb");

        let names: Vec<&str> = env
            .events
            .iter()
            .filter(|e| e.kind == EVENT_SCENE)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["カミナリのフィルター用シーン環境", "シーン環境パラメーター"]
        );
        assert_eq!(env.acv_scenes.len(), 2);

        // m0300's lightning and scene events carry byte-identical +0x30 records
        // (private/tmp/acv_type101_blocks.txt: 01 3c 00 00, 0.2, -0.13, 0.06).
        let first = env.acv_scenes[0]
            .acv_filter
            .expect("m0300 first lightning filter");
        let last = env.acv_scenes[1]
            .acv_filter
            .expect("m0300 last scene filter");
        for filter in [&first, &last] {
            assert!(filter.enabled);
            assert_eq!(filter.kind, 0x3c);
            assert_eq!(filter.sub_kind, [0, 0]);
            assert!(near(filter.params[0], 0.2));
            assert!(near(filter.params[3], -0.13));
            assert!(near(filter.params[5], 0.06));
        }
        assert_eq!(env.acv_first_filter().unwrap().kind, first.kind);
        assert_eq!(
            env.acv_scenes[0].acv_dof.unwrap().params,
            env.acv_scenes[1].acv_dof.unwrap().params
        );

        // `scene` keeps last-event-wins semantics.
        let scene = env.scene.as_ref().expect("ACV core scene");
        assert!(near(scene.far, env.acv_scenes[1].far));
        assert_eq!(scene.acv_filter.unwrap().kind, 0x3c);

        // m0600 is the map where the first filter differs from the last one; the
        // first record must survive even though `scene` is the last event.
        let m0600 = env_from_binder("bind/mission/mission_0006.bnd", "m0600_env.msb");
        assert_eq!(m0600.acv_scenes.len(), 2);
        let lightning = m0600.acv_scenes[0]
            .acv_filter
            .expect("m0600 first lightning filter");
        let normal = m0600.acv_scenes[1]
            .acv_filter
            .expect("m0600 last scene filter");
        assert_eq!(lightning.kind, 0x50);
        assert_eq!(normal.kind, 0xff);
        assert_ne!(lightning.kind, normal.kind);
        assert!(near(lightning.params[0], 0.5));
        assert!(near(lightning.params[3], 0.1));
        assert!(near(lightning.params[4], -0.02));
        assert!(near(lightning.params[5], 0.005));
        assert_eq!(lightning.flags, [0, 0x28, 0, 0, 0]);
        assert!(near(normal.params[0], 0.35));
        assert_eq!(normal.sub_kind, [0x3c, 0x1e]);
        assert_eq!(m0600.scene.as_ref().unwrap().acv_filter.unwrap().kind, 0xff);
        assert_eq!(
            m0600.acv_scenes[0].acv_dof.unwrap().params,
            m0600.acv_scenes[1].acv_dof.unwrap().params
        );
    }

    #[test]
    fn lensflare_assets_and_god_rays_from_disc() {
        let Some(disc) = crate::vfs::test_disc() else {
            return;
        };
        let tpf_bytes =
            crate::vfs::open(&disc, "model/filter/lensflare/lensflare_t.bnd|lensflare.tpf")
                .unwrap();
        let tpf = crate::tpf::read(&tpf_bytes).unwrap();
        let names: Vec<&str> = tpf.textures.iter().map(|t| t.name.as_str()).collect();
        println!("lensflare.tpf ({} textures):", tpf.textures.len());
        for t in &tpf.textures {
            println!("  {} ({}x{}, fmt={:#x})", t.name, t.width, t.height, t.format);
        }
        for i in 0..=10 {
            let expected = format!("lf{i:02}");
            assert!(
                names.iter().any(|n| n.eq_ignore_ascii_case(&expected)),
                "missing {expected} in {names:?}"
            );
        }
        assert!(names.iter().any(|n| n.eq_ignore_ascii_case("lfdust")));

        // Scan all ch_env/*_env.msb files to verify lens_flare / god_ray parsing across maps.
        let mut godray_maps = 0;
        for path in disc.list("model/map/ch_env") {
            if !path.ends_with("_env.msb") {
                continue;
            }
            if let Ok(bytes) = crate::vfs::open(&disc, &path) {
                if let Ok(env) = read(&bytes) {
                    if let Some(sc) = &env.scene {
                        if sc.lens_flare.enabled && sc.lens_flare.god_ray.tex_id >= 0 {
                            godray_maps += 1;
                            if godray_maps <= 5 {
                                println!("god_ray map {path}: {:?}", sc.lens_flare.god_ray);
                            }
                        }
                    }
                }
            }
        }
        println!("total maps with active god_ray: {godray_maps}");
    }

    #[test]
    fn garage_env_from_disc() {
        let Some(disc) = crate::vfs::test_disc() else {
            return;
        };
        // Outdoor Yard (`menu0310`): dark twilight slot-1 world + 10 active point lights + 1 spot.
        let yard = read(
            &crate::vfs::open(&disc, "model/map/ingamegarage/ingamegaragemenu0310_env.msb")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(yard.point_lights.iter().filter(|l| l.enabled).count(), 10);
        assert_eq!(yard.spot_lights.iter().filter(|l| l.enabled).count(), 1);
        let yard_map = yard.light(1).unwrap();
        assert!(near(yard_map.sky[3], 0.0) && near(yard_map.ground[3], 0.0));
        assert!(near(yard_map.fog.density, 0.9) && near(yard_map.fog.range, 400.0));
        let yard_spot = &yard.spot_lights[0];
        let yd = yard_spot.direction();
        // Positioned at (+11.02, 12.79, 0.31), pointing down and toward the AC bay (-X, -Y).
        assert!(yd[0] < -0.3 && yd[1] < -0.85, "unexpected yard spot dir: {yd:?}");
        assert!(near(yard_spot.far, 22.0) && near(yard_spot.fov_y_deg, 70.0));
        let yard_tm = yard.scene.as_ref().unwrap().tonemap;
        assert_eq!(yard_tm.mode, 1);
        assert!(near(yard_tm.lum, 1.0) && near(yard_tm.key, 0.6) && near(yard_tm.mul, 1.1));

        // Warehouse (`menu0300`): 7 active point lights + 1 overhead spot pointing straight down.
        let wh = read(
            &crate::vfs::open(&disc, "model/map/ingamegarage/ingamegaragemenu0300_env.msb")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(wh.point_lights.iter().filter(|l| l.enabled).count(), 7);
        assert_eq!(wh.spot_lights.iter().filter(|l| l.enabled).count(), 1);
        let wd = wh.spot_lights[0].direction();
        assert!(wd[1] < -0.99, "expected straight-down warehouse spot dir: {wd:?}");

        // Underpass (`menu1000`): 9 active point lights, 0 spot lights.
        let up = read(
            &crate::vfs::open(&disc, "model/map/ingamegarage/ingamegaragemenu1000_env.msb")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(up.point_lights.iter().filter(|l| l.enabled).count(), 9);
        assert_eq!(up.spot_lights.iter().filter(|l| l.enabled).count(), 0);
    }
}
