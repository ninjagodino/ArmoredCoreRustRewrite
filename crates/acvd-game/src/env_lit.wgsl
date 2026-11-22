// Map pieces lit like the 360 Flver_ColDif / Flver_ColDifMul pixel shaders (`sheets/map_env.csv`,
// ps.*, ps.multi, and tonemap rows): single or two-texture (UV0/UV1 blended by 1 - vcol.a)
// squared albedo, two directional lights plus a hemisphere ambient, distance and height fog whose
// colours follow the lit colour's luminance, then the c31 = 0.5 halving and m_ToneMapFilter
// (`0x82c18628`).

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
    /// `x = has_diffuse1`, `y = has_diffuse2` (`Map_Diffuse_Multi`).
    flags: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> env: EnvLit;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var diffuse1_tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var diffuse1_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var diffuse2_tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var diffuse2_sampler: sampler;

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

    var vcol = vec4(1.0);
#ifdef VERTEX_COLORS
    vcol = in.color;
#endif

    // Sample diffuse1 directly so vertex alpha (used as blend weight on Map_Diffuse_Multi and
    // sometimes 0 on opaque Map_Diffuse vertices) does not zero out tex1.a.
    var t1 = srgb_encode(pbr.material.base_color.rgb);
    if env.flags.x > 0.5 {
        let s1 = textureSample(diffuse1_tex, diffuse1_sampler, in.uv);
        t1 = srgb_encode(s1.rgb) * s1.a;
    }
    var raw = t1;
    if env.flags.y > 0.5 {
        var uv2 = in.uv;
#ifdef VERTEX_UVS_B
        uv2 = in.uv_b;
#endif
        let s2 = textureSample(diffuse2_tex, diffuse2_sampler, uv2);
        let t2 = srgb_encode(s2.rgb) * s2.a;
        // Flver_ColDifMul.fpo instructions 7, 30, 34: mix(t1, t2, 1 - vcol.a).
        raw = mix(t1, t2, 1.0 - vcol.a);
    }
    raw = raw * vcol.rgb;
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
