// AC part pixel shader: applies the 360 Flver_ACColor* overlay blend (`shader/flver_shader.bnd`
// #56 `Normal\Flver_ACColorColDifEmi.fpo` and #59 `Normal\Flver_ACColorDecalColDifSpcBumpIbl.fpo`)
// using the 7 reversed `FC_AC_OverlayCol` constants (`c180..c186` = [eye, kojima, joint, optional,
// aid2, aid, base], uploaded by `0x82ca9c70` / `0x82bd6588` and `0x82461ce0` after `0x82460ef8`
// remaps RGB into `[color_min, color_max]`), the `<diffuse>_c` mask (`g_AC_ColorMap`, `s13`),
// the camouflage pattern (`g_AC_CamouflageMap`, `s15`, UV scaled by `c190.xy` from `0x82461a98`),
// and the `_Glow` emission map (`g_EmissionMap`, `s6`, scaled by `c138` from `0x82bd7bd8`).

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}

struct AcPaintParams {
    overlay: array<vec4<f32>, 7>,
    camo_scale: vec2<f32>,
    _pad: vec2<f32>,
    emission: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> paint: AcPaintParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var color_map: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var color_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var camo_map: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var emission_map: texture_2d<f32>;

// Undoes the diffuse sampler's sRGB decode so the 360 overlay blend runs in raw texel space.
fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

fn srgb_decode(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3(0.0)) + 0.055) / 1.055, vec3(2.4));
    return select(hi, lo, c <= vec3(0.04045));
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr = pbr_input_from_standard_material(in, is_front);
    pbr.material.base_color = alpha_discard(pbr.material, pbr.material.base_color);

    let mask = textureSample(color_map, color_sampler, in.uv);
    let camo = textureSample(camo_map, color_sampler, in.uv * paint.camo_scale);
    let emi_sample = textureSample(emission_map, color_sampler, in.uv);

    // Flver_ACColor*.fpo instructions 7..18: when `mask.r >= 0.4375` (`c255.w`, armor panel
    // channels `aid2, aid, base`), blend `mask.r` toward `camo.r` by `mask.b`.
    let l = select(mask.r, mix(mask.r, camo.r, mask.b), mask.r >= 0.4375);
    let idx = clamp(i32(floor(l * 8.0 + 0.3)), 0, 6);
    let col = paint.overlay[idx].rgb;

    let d = srgb_encode(pbr.material.base_color.rgb);
    let lo = 2.0 * d * col;
    let hi = 1.0 - 2.0 * (1.0 - d) * (1.0 - col);
    let blended = select(lo, hi, d >= vec3(0.5));
    let painted = mix(d, blended, mask.g);
    pbr.material.base_color = vec4(srgb_decode(painted), pbr.material.base_color.a);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr);

    // Flver_ACColorColDifEmi.fpo instructions 31..37, 54..58, 68:
    // `emi = (emi_tex.rgb * emi_tex.a)^2 * c138.rgb`, added to the lit diffuse colour.
    let emi_t = srgb_encode(emi_sample.rgb) * emi_sample.a;
    out.color = vec4(out.color.rgb + (emi_t * emi_t) * paint.emission.rgb, out.color.a);

    out.color = main_pass_post_lighting_processing(pbr, out.color);
    return out;
}
