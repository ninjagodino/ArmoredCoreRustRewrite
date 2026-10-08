// Map_Sky (NoLightNoFog.spx): the squared texel, unlit and unfogged (sheets/map_env.csv sky
// row). The dome is smaller than the map, so it sits on the far plane behind every other draw.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::VertexOutput,
}

struct SkyOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> SkyOut {
    let pbr = pbr_input_from_standard_material(in, is_front);
    let raw = srgb_encode(pbr.material.base_color.rgb) * pbr.material.base_color.a;
    var out: SkyOut;
    out.color = vec4(raw * raw, 1.0);
    // Reverse Z: 0 is the far plane.
    out.depth = 0.0;
    return out;
}
