// Map_Sky (NoLightNoFog.spx): the squared texel, unlit and unfogged, halved by c31 = 0.5 and
// passed through m_ToneMapFilter (0x82c18628, sheets/map_env.csv sky and tonemap rows). The 360's
// sky pass (0x827f3d38) zeroes the projection's depth row and sets the viewport MinZ to the far
// end, so the dome sits on the far plane behind every other draw.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::VertexOutput,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> tonemap: vec4<f32>;

struct SkyOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

fn apply_tonemap(hdr: vec3<f32>, tm: vec4<f32>) -> vec3<f32> {
    let src = max(hdr, vec3(0.0)) * 0.5;
    let x = src * tm.x;
    let mode = u32(tm.w + 0.5);
    if mode == 0u {
        return sqrt(max(vec3(1.0) - exp2(-x), vec3(0.0))) * tm.y;
    } else if mode == 1u {
        return (x / (x + vec3(1.0))) * tm.y;
    } else if mode == 2u {
        let w = max(tm.z * tm.x, 1e-4);
        let w2 = w * w;
        return (x * (vec3(1.0) + x / w2) / (x + vec3(1.0))) * tm.y;
    }
    return src;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> SkyOut {
    let pbr = pbr_input_from_standard_material(in, is_front);
    let raw = srgb_encode(pbr.material.base_color.rgb) * pbr.material.base_color.a;
    var out: SkyOut;
    out.color = vec4(apply_tonemap(raw * raw, tonemap), 1.0);
    // Reverse Z: 0 is the far plane.
    out.depth = 0.0;
    return out;
}
