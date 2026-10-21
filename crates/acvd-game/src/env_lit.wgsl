// Map pieces lit like the 360 Flver_ColDif pixel shader (sheets/map_env.csv, ps.* and tonemap
// rows): squared albedo, two directional lights plus a hemisphere ambient, then distance and
// height fog whose colours follow the lit colour's luminance, followed by the c31 = 0.5 halving
// and m_ToneMapFilter (0x82c18628).

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::view,
}

struct EnvLit {
    dir0: vec4<f32>,
    dir1: vec4<f32>,
    col0: vec4<f32>,
    col1: vec4<f32>,
    amb_mid: vec4<f32>,
    amb_half: vec4<f32>,
    fog_dist: vec4<f32>,
    fog_height: vec4<f32>,
    fog_col0: vec4<f32>,
    fog_col1: vec4<f32>,
    tonemap: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> env: EnvLit;

// Undoes the sampler's sRGB decode: the 360 squares the raw texel instead.
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

const LUMA: vec3<f32> = vec3(0.29891, 0.58661, 0.11448);

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    let pbr = pbr_input_from_standard_material(in, is_front);
    let base = alpha_discard(pbr.material, pbr.material.base_color);
    let raw = srgb_encode(pbr.material.base_color.rgb) * pbr.material.base_color.a;
    let albedo = raw * raw;

    let n = pbr.N;
    let ambient = max(env.amb_mid.rgb + env.amb_half.rgb * n.y, vec3(0.0));
    let direct = max(
        saturate(dot(n, env.dir0.xyz)) * env.col0.rgb + saturate(dot(n, env.dir1.xyz)) * env.col1.rgb,
        vec3(0.0),
    );
    var col = albedo * (ambient + direct);

    let pos = in.world_position.xyz;
    let d = length((view.world_position - pos).xz);
    let dfog = saturate(env.fog_dist.x * (1.0 - exp2(-(d - env.fog_dist.y) * env.fog_dist.z)));
    let hfog = saturate(env.fog_height.x * saturate(env.fog_height.z * (pos.y - env.fog_height.y)));
    let luma = dot(col, LUMA);
    col = mix(col, env.fog_col0.rgb * (1.0 + env.fog_col0.w * (luma - 1.0)), dfog);
    col = mix(col, env.fog_col1.rgb * (1.0 + env.fog_col1.w * (luma - 1.0)), hfog);

    var out: FragmentOutput;
    out.color = vec4(apply_tonemap(col, env.tonemap), base.a);
    return out;
}
