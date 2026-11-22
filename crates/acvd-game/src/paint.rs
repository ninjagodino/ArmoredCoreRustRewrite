//! AC paint (`param/accolor/color%04d.bin` + `<diffuse>_c` region masks).
//!
//! 360 `0x82331b18` reads `coloring_id` (`+0x4e`) from `AcAssemblyDesignSt` and loads
//! `$(Data)\Param\AcColor\color%04d.bin` (`0x82331478` / `0x82331680`) into `assembly_data + 0xa0`.
//! Each part slot maps to one of the 34 6-colour groups via `0x838a2b58` (`0x83694720`,
//! `0x82463750`, `0x82463798`), with `0x82b4d618` splitting the shared arm FLVER by its root bone
//! (`r_kata` -> group 2 Right Arm, `l_kata` -> group 3 Left Arm).
//!
//! `0x82460ef8` converts each `[R, G, B, A]` byte colour into `[f32; 4]` by linearly remapping
//! RGB into `[color_min, color_max]` (`param/staticdrawparam.bin` fields 147 `+0x228` = `0.2` and
//! 148 `+0x22c` = `0.8`), and `0x82ca9c70` (`0x82bd6588`) + `0x82461ce0` upload the 7 channels into
//! pixel-shader constants `c180..c186` (`FC_AC_OverlayCol[0..7]`) in reverse order:
//! `[eye, kojima, joint, optional, aid2, aid, base]`.
//!
//! When `AcColor.camo[group] > 0`, `0x82460e58` indexes `0x838a2a90` (loaded from
//! `model/image/camouflage/camouflage_t.bnd|camouflage.tpf` by `0x824611b8`) by `camo_id - 1`
//! (`ca0001..ca0011`), binds it to `g_AC_CamouflageMap` (`s15`), and `0x82461a98` uploads
//! `c190 = (mask_w / camo_w, mask_h / camo_h, 1, 1)`.
//!
//! On `_Glow` submeshes (`ACParts_Glow`, `ACParts_Wep_Glow` with `g_EmissionTexture`), `0x82bd8958`
//! (`0x82bd950c`) and `0x82bd7bd8` (`0x82bd7ae8`, `0x82c89c78`) upload `c138` =
//! `(kojima.rgb / 255.0 * 20.0, 1.0)` for `Flver_ACColorColDifEmi.fpo`.

use acvd_data::generated::tuning::STATIC_DRAW_PARAM;
use acvd_formats::accolor::{self, AcColor, GLOW_INTENSITY, OVERLAY_CHANNELS, RIM_INTENSITY};
use acvd_formats::vfs::Disc;
use acvd_render::{LoadedMesh, Packs};
use bevy::asset::embedded_asset;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat};
use bevy::shader::ShaderRef;

pub type AcPaintMaterial = ExtendedMaterial<StandardMaterial, AcPaint>;

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct AcPaint {
    #[uniform(100)]
    pub params: AcPaintParams,
    #[texture(101)]
    #[sampler(102)]
    pub color_map: Handle<Image>,
    #[texture(103)]
    pub camo_map: Handle<Image>,
    #[texture(104)]
    pub emission_map: Handle<Image>,
    #[texture(105)]
    pub specular_map: Handle<Image>,
}

#[derive(ShaderType, Debug, Clone, Copy)]
pub struct AcPaintParams {
    pub overlay: [Vec4; OVERLAY_CHANNELS],
    /// `c190.xy` (`DL_FREG_190`, `0x82461a98`): `mask_size / camo_size`, or `(1, 1)` when `camo == 0`.
    pub camo_scale: Vec2,
    /// `x = 1.0` when a `g_Specular` (`*_s`) texture is bound (`0.0` otherwise); `y = 0.0`.
    pub flags: Vec2,
    /// `c138` (`DL_FREG_138`, `0x82bd7bd8`): `(kojima.rgb / 255.0 * 20.0, 1.0)` on `_Glow` meshes, or `0`.
    pub emission: Vec4,
    /// `c138` (`DL_FREG_138`, `0x82bd8958` / `0x82008994` = `10.0`):
    /// `(kojima.rgb / 255.0 * 10.0, 1.0)` on `_Rim` (`ACParts_Rim*`) meshes, or `0`.
    pub rim: Vec4,
}

impl MaterialExtension for AcPaint {
    fn fragment_shader() -> ShaderRef {
        "embedded://acvd_game/ac_paint.wgsl".into()
    }
}

pub struct PaintPlugin;

impl Plugin for PaintPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "ac_paint.wgsl");
        app.add_plugins(MaterialPlugin::<AcPaintMaterial>::default());
    }
}

/// Load `param/accolor/color{coloring_id:04}.bin` for a design (`0x82331478` / `0x82331b18`).
pub fn load_accolor(disc: &Disc, coloring_id: u16) -> Option<AcColor> {
    if coloring_id == 0 {
        return None;
    }
    let path = format!("param/accolor/color{coloring_id:04}.bin");
    let bytes = disc.asset(&path).ok()?;
    match accolor::read(&bytes) {
        Ok(c) => Some(c),
        Err(e) => {
            warn!("{path}: {e:#}");
            None
        }
    }
}

/// Look up `<diffuse>_c` in the same TPF pack (or across texture packs) with a linear (non-sRGB)
/// format so the pixel shader reads the exact 5-bit `R` channel index and `G` weight.
pub fn load_color_map(disc: &Disc, packs: &mut Packs, diffuse: &str) -> Option<Image> {
    let mask_name = format!("{diffuse}_c");
    let mut img = if let Some(d_ref) = acvd_data::textures_named(diffuse).next() {
        let idx = packs.find(disc, d_ref.pack, &mask_name).ok().flatten();
        idx.and_then(|i| packs.texture_at(disc, d_ref.pack, i, &mask_name).ok())
    } else {
        None
    }
    .or_else(|| packs.image(disc, &mask_name).ok())?;

    img.texture_descriptor.format = match img.texture_descriptor.format {
        TextureFormat::Bc1RgbaUnormSrgb => TextureFormat::Bc1RgbaUnorm,
        TextureFormat::Bc3RgbaUnormSrgb => TextureFormat::Bc3RgbaUnorm,
        other => other,
    };
    Some(img)
}

/// Load camouflage pattern `ca{camo_id:04}` from `model/image/camouflage/camouflage_t.bnd|camouflage.tpf`
/// (`0x824611b8` / `0x82460e58`) with a linear format so its 5-bit `R` channel decodes directly into
/// palette indices `3..=6` (`optional, aid2, aid, base`).
pub fn load_camo_map(disc: &Disc, packs: &mut Packs, camo_id: u8) -> Option<Image> {
    if camo_id == 0 {
        return None;
    }
    let name = format!("ca{camo_id:04}");
    let mut img = packs.image(disc, &name).ok()?;
    img.texture_descriptor.format = match img.texture_descriptor.format {
        TextureFormat::Bc1RgbaUnormSrgb => TextureFormat::Bc1RgbaUnorm,
        TextureFormat::Bc3RgbaUnormSrgb => TextureFormat::Bc3RgbaUnorm,
        other => other,
    };
    Some(img)
}

/// True when the FLVER material MTD requests `_Rim` Fresnel highlights (`0x82bd8198` /
/// `0x82bd8958`, checking substring `_rim` on `ACParts_Rim`, `ACParts_Rim_ex`, etc.).
pub fn is_rim_mtd(mtd: &str) -> bool {
    mtd.to_ascii_lowercase().contains("_rim")
}

/// 1x1 zero RGBA image used as a neutral mask when a `_Rim` mesh has no `<diffuse>_c` mask.
fn empty_mask_image() -> Image {
    let mut img = Image::default();
    img.texture_descriptor.size = Extent3d {
        width: 1,
        height: 1,
        depth_or_array_layers: 1,
    };
    img.texture_descriptor.dimension = TextureDimension::D2;
    img.texture_descriptor.format = TextureFormat::Rgba8Unorm;
    img.data = Some(vec![0, 0, 0, 0]);
    img
}

/// Build an [`AcPaintMaterial`] when `accolor` is present and the mesh has a `<diffuse>_c` mask
/// or a `_Rim` material (`ACParts_Rim*`).
pub fn material(
    disc: &Disc,
    base: &StandardMaterial,
    mesh: &LoadedMesh,
    column: &str,
    accolor: &AcColor,
    packs: &mut Packs,
    images: &mut Assets<Image>,
) -> Option<AcPaintMaterial> {
    let is_rim = is_rim_mtd(&mesh.mtd);
    let mask = mesh
        .diffuse
        .as_deref()
        .and_then(|d| load_color_map(disc, packs, d));
    let mask = match mask {
        Some(m) => m,
        None if is_rim => empty_mask_image(),
        None => return None,
    };
    let mask_size = Vec2::new(
        mask.texture_descriptor.size.width as f32,
        mask.texture_descriptor.size.height as f32,
    );
    let group = accolor::slot_group(column, mesh.root.as_deref());
    let cols = accolor.overlay_cols(group, STATIC_DRAW_PARAM.color_min, STATIC_DRAW_PARAM.color_max);
    let color_map = images.add(mask);

    // 360 `0x82ca9c70` (`0x82ca9e98..0x82ca9f14`) binds `ca{camo_id:04}` (`0x82460e58`) to `s15`
    // with `c190.xy = mask_size / camo_size` (`0x82461a98`), or binds `color_map` itself with
    // `c190 = (1, 1, 1, 1)` when `camo_id == 0`.
    let (camo_map, camo_scale) = if let Some(camo) = load_camo_map(disc, packs, accolor.camo_id(group)) {
        let camo_w = (camo.texture_descriptor.size.width as f32).max(1.0);
        let camo_h = (camo.texture_descriptor.size.height as f32).max(1.0);
        (
            images.add(camo),
            Vec2::new(mask_size.x / camo_w, mask_size.y / camo_h),
        )
    } else {
        (color_map.clone(), Vec2::ONE)
    };

    let (emission_map, emission) = match mesh
        .emission
        .as_deref()
        .and_then(|name| packs.image(disc, name).ok())
    {
        Some(emi) => (
            images.add(emi),
            Vec4::from(accolor.emission_col(group, GLOW_INTENSITY)),
        ),
        None => (color_map.clone(), Vec4::ZERO),
    };

    let (specular_map, has_specular) = match mesh
        .specular
        .as_deref()
        .and_then(|name| packs.linear_image(disc, name).ok())
    {
        Some(spc) => (images.add(spc), 1.0),
        None => (color_map.clone(), 0.0),
    };

    let rim = if is_rim {
        Vec4::from(accolor.emission_col(group, RIM_INTENSITY))
    } else {
        Vec4::ZERO
    };

    Some(AcPaintMaterial {
        base: base.clone(),
        extension: AcPaint {
            params: AcPaintParams {
                overlay: cols.map(Vec4::from),
                camo_scale,
                flags: Vec2::new(has_specular, 0.0),
                emission,
                rim,
            },
            color_map,
            camo_map,
            emission_map,
            specular_map,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color5001_overlay_constants_match_360_order() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let iso = acvd_formats::vfs::default_disc(&root);
        if !iso.is_file() {
            return;
        }
        let disc = Disc::open(&iso).unwrap();
        let col = load_accolor(&disc, 5001).expect("color5001.bin");
        // Group 0 (Head): base=(75,155,85), aid=(130,90,55), aid2=(119,91,70),
        // optional=(100,110,100), joint=(18,18,18), kojima=(251,203,188); eye=(0,0,0).
        let ov = col.overlay_cols(0, STATIC_DRAW_PARAM.color_min, STATIC_DRAW_PARAM.color_max);
        // c180 = eye (0, 0, 0) -> [0.2, 0.2, 0.2, 1.0]
        assert_eq!(ov[0], [0.2, 0.2, 0.2, 1.0]);
        // c181 = kojima (251, 203, 188) -> R > G > B
        assert!(ov[1][0] > ov[1][1] && ov[1][1] > ov[1][2], "kojima {ov:?}");
        // c186 = base (75, 155, 85) -> G = 0.2 + 0.6 * (155/255) = 0.5647059
        assert!((ov[6][1] - (0.2 + 0.6 * (155.0 / 255.0))).abs() < 1e-5);
        // Emission c138 = kojima (251, 203, 188) / 255.0 * 20.0
        let emi = col.emission_col(0, GLOW_INTENSITY);
        assert!((emi[0] - (251.0 / 255.0) * 20.0).abs() < 1e-4);
        assert_eq!(emi[3], 1.0);
    }

    #[test]
    fn loads_camouflage_patterns_as_linear_bc1() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let iso = acvd_formats::vfs::default_disc(&root);
        if !iso.is_file() {
            return;
        }
        let disc = Disc::open(&iso).unwrap();
        let mut packs = Packs::default();
        for camo_id in 1..=11 {
            let img = load_camo_map(&disc, &mut packs, camo_id)
                .unwrap_or_else(|| panic!("missing ca{camo_id:04}"));
            assert_eq!(img.texture_descriptor.size.width, 256);
            assert_eq!(img.texture_descriptor.size.height, 256);
            assert_eq!(img.texture_descriptor.format, TextureFormat::Bc1RgbaUnorm);
        }
        let c5011 = load_accolor(&disc, 5011).expect("color5011.bin");
        assert_eq!(c5011.camo_id(0), 1);
        let c6002 = load_accolor(&disc, 6002).expect("color6002.bin");
        assert_eq!(c6002.camo_id(0), 6);
    }

    #[test]
    fn rim_and_specular_material_params_bind_correctly() {
        assert!(is_rim_mtd("ACParts_Rim_ex"));
        assert!(is_rim_mtd("ACParts_Rim.mtd"));
        assert!(!is_rim_mtd("ACParts"));
        assert!(!is_rim_mtd("ACParts_Glow"));

        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let iso = acvd_formats::vfs::default_disc(&root);
        if !iso.is_file() {
            return;
        }
        let disc = Disc::open(&iso).unwrap();
        let col = load_accolor(&disc, 5001).expect("color5001.bin");
        let mut meshes = acvd_render::model(&disc, "model/ac/parts/arm/am0010/am0010_m.bnd.dcx|am0010.flv").unwrap();
        let mut packs = Packs::default();
        let mut images = Assets::<Image>::default();
        let mut missing = 0;

        // Standard AC armor mesh: binds normal map on base and specular map on AcPaint.
        let base = acvd_render::material(&disc, &meshes[0], &mut packs, &mut images, false, &mut missing);
        assert!(base.normal_map_texture.is_some(), "am0010 base should bind normal_map_texture");
        let painted = material(&disc, &base, &meshes[0], "arms", &col, &mut packs, &mut images)
            .expect("am0010 painted material");
        assert_eq!(painted.extension.params.flags.x, 1.0, "specular map bound");
        assert_eq!(painted.extension.params.rim, Vec4::ZERO, "non-rim mesh has zero rim");

        // Simulated `_Rim` MTD: uploads kojima * 10.0 (`RIM_INTENSITY`, 0x82008994 in 0x82bd8958).
        meshes[0].mtd = "ACParts_Rim_ex".into();
        let rim_mat = material(&disc, &base, &meshes[0], "head", &col, &mut packs, &mut images)
            .expect("rim painted material");
        let expected_rim = col.emission_col(0, RIM_INTENSITY);
        assert_eq!(rim_mat.extension.params.rim, Vec4::from(expected_rim));
        assert!((rim_mat.extension.params.rim.x - (251.0 / 255.0) * 10.0).abs() < 1e-4);
    }
}
