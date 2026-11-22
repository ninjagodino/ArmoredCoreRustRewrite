// Water surface (`Water.spx`, `Normal/Flver_Water.vpo` + `Normal/Flver_Water.fpo`,
// `material/m4000_mtd.bnd` `Water.mtd`, `sheets/map_env.csv` `ps.water` row):
// three world-XZ scrolled normal-map layers (`water_n`), Schlick-style Fresnel blending deep
// `g_WaterColor` with sky/horizon reflection, two directional specular lobes (`c11`/`c12`),
// distance + height fog with luminance tint, and `m_ToneMapFilter`.

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::view,
}

struct WaterParams {
    dir0: vec4<f32>,
    dir1: vec4<f32>,
    spec0: vec4<f32>,
    spec1: vec4<f32>,
    amb_mid: vec4<f32>,
    amb_half: vec4<f32>,
    fog_dist: vec4<f32>,
    fog_height: vec4<f32>,
    fog_col0: vec4<f32>,
    fog_col1: vec4<f32>,
    tonemap: vec4<f32>,
    water_color: vec4<f32>,
    /// `(g_FresnelPow, g_FresnelBias, g_FresnelScale, g_SpecularPower)`.
    fresnel: vec4<f32>,
    /// `(g_TileScale_0, g_TileScale_1, g_TileScale_2, g_BumpMapSmoose)`.
    tile_scale: vec4<f32>,
    /// `(scroll0.x, scroll0.y, scroll1.x, scroll1.y)`.
    scroll01: vec4<f32>,
    /// `(scroll2.x, scroll2.y, time_secs, has_bump)`.
    scroll2_time: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> water: WaterParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var bump_tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var bump_sampler: sampler;

fn unpack_normal(uv: vec2<f32>) -> vec3<f32> {
    let xy = textureSample(bump_tex, bump_sampler, uv).rg * 2.0 - vec2(1.0);
    let z = sqrt(max(1.0 - dot(xy, xy), 0.0));
    return vec3(xy.x, z, xy.y);
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
fn fragment(in: VertexOutput) -> FragmentOutput {
    let pos = in.world_position.xyz;
    let t = water.scroll2_time.z;

    // Flver_Water.vpo o2.xy = world.xz; Flver_Water.fpo samples g_BumpMap 3x and smooths Z.
    var n = normalize(in.world_normal);
    if water.scroll2_time.w > 0.5 {
        let xz = pos.xz;
        let n0 = unpack_normal(xz * water.tile_scale.x + water.scroll01.xy * t);
        let n1 = unpack_normal(xz * water.tile_scale.y + water.scroll01.zw * t);
        let n2 = unpack_normal(xz * water.tile_scale.z + water.scroll2_time.xy * t);
        let sum = n0 + n1 + n2;
        n = normalize(vec3(sum.x, sum.y + water.tile_scale.w, sum.z));
    }

    let v = normalize(view.world_position - pos);
    let r = reflect(-v, n);

    // Flver_Water.fpo instructions 67-77: Fresnel blends deep water color toward sky reflection.
    let ndotv = clamp(dot(n, v), 0.0, 1.0);
    let f = pow(1.0 - ndotv, water.fresnel.x);
    let k = mix(water.fresnel.y, 1.0, f) * water.fresnel.z;

    let sky_ref = max(water.amb_mid.rgb + water.amb_half.rgb * clamp(r.y, -1.0, 1.0), vec3(0.0));
    let horizon = pow(1.0 - clamp(abs(r.y), 0.0, 1.0), 3.0);
    let refl = mix(sky_ref, water.fog_col0.rgb * 0.85, horizon * 0.5);
    let deep = water.water_color.rgb * max(water.amb_mid.rgb, vec3(0.25));
    var col = mix(deep, refl, k);

    // Flver_Water.fpo instructions 79-87: directional specular lobes from c11 / c12.
    let s0 = pow(saturate(dot(r, water.dir0.xyz)), water.fresnel.w) * water.spec0.rgb;
    let s1 = pow(saturate(dot(r, water.dir1.xyz)), water.fresnel.w) * water.spec1.rgb;
    col = col + (s0 + s1) * k;

    // Flver_Water.fpo instructions 89-101: distance + height fog with luma tint, then tonemap.
    let d = length((view.world_position - pos).xz);
    let dfog = saturate(water.fog_dist.x * (1.0 - exp2(-(d - water.fog_dist.y) * water.fog_dist.z)));
    let hfog = saturate(water.fog_height.x * saturate(water.fog_height.z * (pos.y - water.fog_height.y)));
    let luma = dot(col, LUMA);
    col = mix(col, water.fog_col0.rgb * (1.0 + water.fog_col0.w * (luma - 1.0)), dfog);
    col = mix(col, water.fog_col1.rgb * (1.0 + water.fog_col1.w * (luma - 1.0)), hfog);

    var out: FragmentOutput;
    out.color = vec4(apply_tonemap(col, water.tonemap), 1.0);
    return out;
}
