// AC part pixel shader: applies the 360 Flver_ACColor* overlay blend (`shader/flver_shader.bnd`
// #53 `Normal\Flver_ACColorColDifEmi.fpo` and #56 `Normal\Flver_ACColorDecalColDifSpcBumpIbl.fpo`)
// using the 7 reversed `FC_AC_OverlayCol` constants (`c180..c186` = [eye, kojima, joint, optional,
// aid2, aid, base], uploaded by `0x82ca9c70` / `0x82bd6588` and `0x82461ce0` after `0x82460ef8`
// remaps RGB into `[color_min, color_max]`) and the `<diffuse>_c` mask (`g_AC_ColorMap`).

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
}

struct AcPaintParams {
    overlay: array<vec4<f32>, 7>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> paint: AcPaintParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var color_map: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var color_sampler: sampler;

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
    let idx = clamp(i32(floor(mask.r * 8.0 + 0.3)), 0, 6);
    let col = paint.overlay[idx].rgb;

    let d = srgb_encode(pbr.material.base_color.rgb);
    let lo = 2.0 * d * col;
    let hi = 1.0 - 2.0 * (1.0 - d) * (1.0 - col);
    let blended = select(lo, hi, d >= vec3(0.5));
    let painted = mix(d, blended, mask.g);
    pbr.material.base_color = vec4(srgb_decode(painted), pbr.material.base_color.a);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr);
    out.color = main_pass_post_lighting_processing(pbr, out.color);
    return out;
}
