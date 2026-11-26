//! Action-43 heat-haze / distortion (`FXParticleAppearance_Distortion`).
//!
//! 360 trail (all static):
//! - `0x83199ab8` registers action `0x2b` (43) with factory `0x83198570`, parser
//!   `0x831992d0` and draw `0x83198820`.
//! - Factory `0x835b7330` reads the 24 FFX slots in order; `0x835b9580` builds the
//!   camera-facing quad from slots 3/4 (size), 6..13 (shader constants), 14 (normal map)
//!   and 16 (colour). Slots 9/10 offset the normal-map sample.
//! - Shader family `shader/ffx_shader.bnd|Sfx_DistortionType{0..5}`; f0000300 slot 0 = 1
//!   selects technique `0xd` = `Sfx_DistortionType1`: `DL_FREG_006/007/009/010/020` are
//!   slots 6/7/9/10/16, `s1 = NormalSampler` (DXN `s1206_n`), diffuse sampler = the scene.
//! - The shader normal map is sampled at `uv + (c9,c10) * c6`, unpacked `* 2 - 1`, and the
//!   scene is sampled at `screen_uv + n * c7 * falloff`, with `falloff = max(0, 1 - r * c6.y)`.
//!   `c20` tints the result; the pass alpha-blends it over the framebuffer.
//!
//! Bevy's `FullscreenMaterial` pass owns the scene texture. The 32x32 normal table below is
//! the CPU-decoded `s1206_n` (DXN/BC5) mip 0, bilinearly sampled in `haze.wgsl`; it rides in
//! the pass uniform so no second texture binding is needed.

use bevy::asset::embedded_asset;
use bevy::core_pipeline::fullscreen_material::FullscreenMaterial;
use bevy::core_pipeline::tonemapping::tonemapping;
use bevy::core_pipeline::Core3dSystems;
use bevy::ecs::schedule::ScheduleConfigs;
use bevy::ecs::system::BoxedSystem;
use bevy::post_process::bloom::bloom;
use bevy::prelude::*;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::{ShaderType, TextureFormat};
use bevy::shader::ShaderRef;

/// Maximum distortion quads in one frame.
const MAX_HAZE: usize = 16;
/// Edge of the CPU normal table (`NORMAL_N * NORMAL_N` entries).
const NORMAL_N: usize = 32;
const NORMAL_TABLE: usize = NORMAL_N * NORMAL_N;
/// `s1206_n` is the normal map every distortion effect in `sfx/acv_commoneffects.ffxbnd`
/// uses except two (1038 / 0); keep the trace simple and sample 1206.
const NORMAL_NAME: &str = "s1206_n";

#[derive(Clone, Copy, ShaderType, Default)]
struct HazeQuad {
    /// Screen UV of the quad centre (top-left origin, 0..1).
    center: Vec2,
    /// Half-axis for local +X, in screen UV.
    axis_u: Vec2,
    /// Half-axis for local +Y, in screen UV.
    axis_v: Vec2,
    /// View depth in metres (used only for ordering).
    depth: f32,
    /// Shader `c7.x`: scene-UV offset scale.
    strength: f32,
    /// Shader `c6.y`: edge falloff `max(0, 1 - r * power)`.
    power: f32,
    /// Shader `c9.x` / `c10.x`: normal-map UV phase.
    normal_scale: Vec2,
    /// Shader `c20`: output tint/alpha.
    color: Vec4,
}

/// Per-camera action-43 pass. `normals` is the decoded `s1206_n` table; `quads` is refreshed
/// from the live [`crate::sfx::HazeFx`] markers every frame.
#[derive(Component, ExtractComponent, Clone, Copy, ShaderType)]
pub struct HazePass {
    quads: [HazeQuad; MAX_HAZE],
    count: u32,
    flags: u32,
    normals: [Vec4; NORMAL_TABLE],
}

impl Default for HazePass {
    fn default() -> Self {
        Self {
            quads: [HazeQuad::default(); MAX_HAZE],
            count: 0,
            flags: 0,
            normals: [Vec4::ZERO; NORMAL_TABLE],
        }
    }
}

impl FullscreenMaterial for HazePass {
    fn fragment_shader() -> ShaderRef {
        "embedded://acvd_game/haze.wgsl".into()
    }

    fn schedule_configs(system: ScheduleConfigs<BoxedSystem>) -> ScheduleConfigs<BoxedSystem> {
        // The 360 draws distortion in the transparent particle pass, before bloom and the
        // tone-map; keep it ahead of both Bevy post passes.
        system
            .in_set(Core3dSystems::PostProcess)
            .before(bloom)
            .before(tonemapping)
    }
}

#[derive(Resource)]
struct HazeNormals([Vec4; NORMAL_TABLE]);

pub struct HazePlugin;

impl Plugin for HazePlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "haze.wgsl");
        app.add_plugins(crate::fullscreen_extra::ExtraFullscreenMaterialPlugin::<
            HazePass,
        >::default())
            .add_systems(Startup, load_normals)
            .add_systems(PostUpdate, (ensure_pass, update_haze).chain());
    }
}

fn ensure_pass(
    mut commands: Commands,
    cams: Query<
        Entity,
        (
            With<Camera3d>,
            Without<HazePass>,
            Without<crate::env::WaterReflectionCamera>,
        ),
    >,
) {
    for e in &cams {
        commands.entity(e).insert(HazePass::default());
    }
}

fn load_normals(mut commands: Commands, dir: Res<crate::sfx::Dir>) {
    let mut packs = acvd_render::Packs::default();
    let table = packs
        .linear_image(&dir.0, NORMAL_NAME)
        .ok()
        .and_then(|img| normal_table(&img))
        .unwrap_or_else(|| {
            warn!("haze: {NORMAL_NAME} not available, using the fallback normal table");
            fallback_table()
        });
    commands.insert_resource(HazeNormals(table));
}

fn update_haze(
    normals: Option<Res<HazeNormals>>,
    mut cams: Query<
        (&GlobalTransform, &Camera, &mut HazePass),
        Without<crate::env::WaterReflectionCamera>,
    >,
    hazes: Query<(&GlobalTransform, &crate::sfx::HazeFx)>,
) {
    let Some(normals) = normals else { return };
    let mut list: Vec<(f32, HazeQuad)> = Vec::with_capacity(MAX_HAZE);
    for (cam_gt, camera, mut pass) in &mut cams {
        if pass.flags & 1 == 0 {
            pass.normals = normals.0;
            pass.flags |= 1;
        }
        pass.count = 0;
        if hazes.is_empty() {
            continue;
        }
        let cam_pos = cam_gt.translation();
        let cam_rot = cam_gt.rotation();
        let right = cam_rot * Vec3::X;
        let up = cam_rot * Vec3::Y;
        let forward = cam_rot * Vec3::NEG_Z;
        list.clear();
        for (hgt, h) in &hazes {
            let center = hgt.translation();
            let depth = (center - cam_pos).dot(forward);
            if depth <= 0.1 {
                continue;
            }
            let (sin, cos) = h.angle.sin_cos();
            let u = (right * cos + up * sin) * (h.width * 0.5);
            let v = (-right * sin + up * cos) * (h.height * 0.5);
            let Some(c0) = project(camera, cam_gt, center) else {
                continue;
            };
            let Some(cu) = project(camera, cam_gt, center + u) else {
                continue;
            };
            let Some(cv) = project(camera, cam_gt, center + v) else {
                continue;
            };
            list.push((
                depth,
                HazeQuad {
                    center: c0,
                    axis_u: cu - c0,
                    axis_v: cv - c0,
                    depth,
                    strength: h.strength,
                    power: h.power,
                    normal_scale: h.normal_scale,
                    color: h.color,
                },
            ));
        }
        // Farthest first; nearest replaces it in the shader's sequence.
        list.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (i, (_, q)) in list.iter().take(MAX_HAZE).enumerate() {
            pass.quads[i] = *q;
        }
        pass.count = list.len().min(MAX_HAZE) as u32;
    }
}

fn project(camera: &Camera, cam_gt: &GlobalTransform, p: Vec3) -> Option<Vec2> {
    let ndc = camera.world_to_ndc(cam_gt, p)?;
    if !(0.0..=1.0).contains(&ndc.z) {
        return None;
    }
    Some(Vec2::new(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5))
}

fn normal_table(img: &Image) -> Option<[Vec4; NORMAL_TABLE]> {
    let w = img.texture_descriptor.size.width as usize;
    let h = img.texture_descriptor.size.height as usize;
    if w == 0 || h == 0 {
        return None;
    }
    let data = img.data.as_ref()?;
    let fmt = img.texture_descriptor.format;
    let decoded: Vec<[u8; 2]> = match fmt {
        TextureFormat::Bc5RgUnorm => decode_bc5(data, w, h, false)?,
        TextureFormat::Bc5RgSnorm => decode_bc5(data, w, h, true)?,
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb => data
            .chunks_exact(4)
            .take(w * h)
            .map(|p| [p[0], p[1]])
            .collect(),
        TextureFormat::Rg8Unorm => data
            .chunks_exact(2)
            .take(w * h)
            .map(|p| [p[0], p[1]])
            .collect(),
        _ => {
            warn!("haze: unsupported {NORMAL_NAME} format {fmt:?}");
            return None;
        }
    };
    if decoded.len() < w * h {
        return None;
    }
    let mut table = [Vec4::ZERO; NORMAL_TABLE];
    for y in 0..NORMAL_N {
        for x in 0..NORMAL_N {
            let sx = ((x as f32 + 0.5) / NORMAL_N as f32 * w as f32) as usize;
            let sy = ((y as f32 + 0.5) / NORMAL_N as f32 * h as f32) as usize;
            let sx = sx.min(w - 1);
            let sy = sy.min(h - 1);
            let mut acc = Vec2::ZERO;
            let mut n = 0.0;
            for dy in 0..2 {
                for dx in 0..2 {
                    let px = (sx + dx).min(w - 1);
                    let py = (sy + dy).min(h - 1);
                    let p = decoded[py * w + px];
                    acc += Vec2::new(p[0] as f32 / 255.0, p[1] as f32 / 255.0);
                    n += 1.0;
                }
            }
            let v = acc / n;
            table[y * NORMAL_N + x] = Vec4::new(v.x, v.y, 0.0, 0.0);
        }
    }
    Some(table)
}

fn fallback_table() -> [Vec4; NORMAL_TABLE] {
    let mut table = [Vec4::ZERO; NORMAL_TABLE];
    for y in 0..NORMAL_N {
        for x in 0..NORMAL_N {
            let u = x as f32 / NORMAL_N as f32 * std::f32::consts::TAU;
            let v = y as f32 / NORMAL_N as f32 * std::f32::consts::TAU;
            let nx = 0.5 + 0.25 * (3.0 * u + 2.0 * v).sin() + 0.15 * (7.0 * u).cos();
            let ny = 0.5 + 0.25 * (3.0 * v - 2.0 * u).cos() + 0.15 * (5.0 * v).sin();
            table[y * NORMAL_N + x] = Vec4::new(nx.clamp(0.0, 1.0), ny.clamp(0.0, 1.0), 0.0, 0.0);
        }
    }
    table
}

fn decode_bc5(data: &[u8], w: usize, h: usize, snorm: bool) -> Option<Vec<[u8; 2]>> {
    let bw = w.div_ceil(4);
    let bh = h.div_ceil(4);
    if data.len() < bw * bh * 16 {
        return None;
    }
    let mut out = vec![[128u8, 128u8]; w * h];
    for by in 0..bh {
        for bx in 0..bw {
            let off = (by * bw + bx) * 16;
            let r = decode_bc4(&data[off..off + 8], snorm);
            let g = decode_bc4(&data[off + 8..off + 16], snorm);
            for y in 0..4 {
                for x in 0..4 {
                    let px = bx * 4 + x;
                    let py = by * 4 + y;
                    if px < w && py < h {
                        out[py * w + px] = [r[y * 4 + x], g[y * 4 + x]];
                    }
                }
            }
        }
    }
    Some(out)
}

fn decode_bc4(block: &[u8], snorm: bool) -> [u8; 16] {
    let e0 = block[0];
    let e1 = block[1];
    let mut pal = [0u8; 8];
    let (a, b) = if snorm {
        (e0 as i8 as f32 / 127.0, e1 as i8 as f32 / 127.0)
    } else {
        (e0 as f32, e1 as f32)
    };
    let to_u8 = |v: f32| {
        if snorm {
            ((v * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            v.clamp(0.0, 255.0).round() as u8
        }
    };
    pal[0] = to_u8(a);
    pal[1] = to_u8(b);
    let hi = if snorm {
        (e0 as i8) > (e1 as i8)
    } else {
        e0 > e1
    };
    if hi {
        for i in 1..7 {
            pal[i + 1] = to_u8(a + (b - a) * i as f32 / 7.0);
        }
    } else {
        for i in 1..5 {
            pal[i + 1] = to_u8(a + (b - a) * i as f32 / 5.0);
        }
        pal[6] = to_u8(if snorm { -1.0 } else { 0.0 });
        pal[7] = to_u8(if snorm { 1.0 } else { 255.0 });
    }
    let mut bits = 0u64;
    for (i, b) in block[2..8].iter().enumerate() {
        bits |= (*b as u64) << (8 * i);
    }
    let mut out = [0u8; 16];
    for (i, v) in out.iter_mut().enumerate() {
        *v = pal[((bits >> (3 * i)) & 7) as usize];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_formats::vfs::Disc;

    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn s1206_n_decodes_to_a_varied_normal_table() {
        let Ok(disc) = Disc::default_for(&root()) else {
            return;
        };
        let mut packs = acvd_render::Packs::default();
        let img = packs
            .linear_image(&disc, NORMAL_NAME)
            .expect("s1206_n on the disc");
        let table = normal_table(&img).expect("DXN table");
        let min = table.iter().map(|v| v.x).fold(1.0f32, f32::min);
        let max = table.iter().map(|v| v.x).fold(0.0f32, f32::max);
        assert!(max - min > 0.1, "normal table is flat: {min}..{max}");
        assert!(table.iter().all(|v| (0.0..=1.0).contains(&v.x)));
        assert!(table.iter().all(|v| (0.0..=1.0).contains(&v.y)));
    }

    #[test]
    fn fallback_table_is_tileable_and_non_flat() {
        let t = fallback_table();
        let min = t.iter().map(|v| v.x).fold(1.0f32, f32::min);
        let max = t.iter().map(|v| v.x).fold(0.0f32, f32::max);
        assert!(max - min > 0.1);
    }
}
