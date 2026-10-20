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

use acvd_data::generated::tuning::STATIC_DRAW_PARAM;
use acvd_formats::accolor::{self, AcColor, OVERLAY_CHANNELS};
use acvd_formats::vfs::Disc;
use acvd_render::{LoadedMesh, Packs};
use bevy::asset::embedded_asset;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType, TextureFormat};
use bevy::shader::ShaderRef;

pub type AcPaintMaterial = ExtendedMaterial<StandardMaterial, AcPaint>;

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct AcPaint {
    #[uniform(100)]
    pub params: AcPaintParams,
    #[texture(101)]
    #[sampler(102)]
    pub color_map: Handle<Image>,
}

#[derive(ShaderType, Debug, Clone, Copy)]
pub struct AcPaintParams {
    pub overlay: [Vec4; OVERLAY_CHANNELS],
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

/// Build an [`AcPaintMaterial`] when `accolor` is present and the mesh has a `<diffuse>_c` mask.
pub fn material(
    disc: &Disc,
    base: &StandardMaterial,
    mesh: &LoadedMesh,
    column: &str,
    accolor: &AcColor,
    packs: &mut Packs,
    images: &mut Assets<Image>,
) -> Option<AcPaintMaterial> {
    let diffuse = mesh.diffuse.as_deref()?;
    let mask = load_color_map(disc, packs, diffuse)?;
    let group = accolor::slot_group(column, mesh.root.as_deref());
    let cols = accolor.overlay_cols(group, STATIC_DRAW_PARAM.color_min, STATIC_DRAW_PARAM.color_max);
    Some(AcPaintMaterial {
        base: base.clone(),
        extension: AcPaint {
            params: AcPaintParams {
                overlay: cols.map(Vec4::from),
            },
            color_map: images.add(mask),
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
    }
}
