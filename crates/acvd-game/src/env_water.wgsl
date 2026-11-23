// Water surface (`Water.spx`, `Normal/Flver_Water.vpo` + `Normal/Flver_Water.fpo`,
// `material/m4000_mtd.bnd` `Water.mtd`, `sheets/map_env.csv` `ps.water` row):
// three world-XZ scrolled normal-map layers (`water_n`), mirrored planar reflection render
// target (`s0` `g_DiffuseMap`, already linear HDR since the reflection camera runs without
// `ZoomBlur`) distorted by `n.xz * g_ReflectBand`, tinted by `g_FresnelColor`, blended over
// deep `g_WaterColor` via Fresnel (`g_FresnelPow` / `g_FresnelBias` / `g_FresnelScale`), plus
// two directional specular lobes (`c11`/`c12`) and distance + height fog with luminance tint.
// Outputs linear HDR for fullscreen `m_ToneMapFilter` (`zoom_blur.wgsl`).

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::view,
}

struct WaterParams {
    dir0: vec4<f32>,
    dir1: vec4<f32>,
    spec0: vec4<f32>,
    spec1: vec4<f32>,
    /// `g_FresnelColor.rgb` (`c132`, `0.4784`) and `w = g_ReflectBand` (`c124`, `0.2`).
    fresnel_color: vec4<f32>,
    /// Fallback reflection colour when no planar reflection target is active (`w = has_refl`).
    fallback_refl: vec4<f32>,
    fog_dist: vec4<f32>,
    fog_height: vec4<f32>,
    fog_col0: vec4<f32>,
    fog_col1: vec4<f32>,
    /// `g_WaterColor` (`c127`, `Water.mtd`: `(0.15, 0.10, 0.05)` squared for 360 linear space).
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
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var refl_tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var refl_sampler: sampler;

fn unpack_normal(uv: vec2<f32>) -> vec3<f32> {
    let xy = textureSample(bump_tex, bump_sampler, uv).rg * 2.0 - vec2(1.0);
    let z = sqrt(max(1.0 - dot(xy, xy), 0.0));
    return vec3(xy.x, z, xy.y);
}

const LUMA: vec3<f32> = vec3(0.29891, 0.58661, 0.11448);

@fragment
fn fragment(in: VertexOutput) -> FragmentOutput {
    let pos = in.world_position.xyz;
    let t = water.scroll2_time.z;

    // Flver_Water.vpo o2.xy = world.xz; Flver_Water.fpo inst 9-47 samples g_BumpMap 3x + smooths Z.
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

    // Flver_Water.fpo inst 49-50 & 74: sample mirrored planar reflection (linear HDR) distorted
    // by g_ReflectBand, then tint by g_FresnelColor (`c132`).
    var refl_hdr = water.fallback_refl.rgb;
    if water.fallback_refl.w > 0.5 {
        let screen_uv = (in.position.xy - view.viewport.xy) / max(view.viewport.zw, vec2(1.0));
        let distort = n.xz * water.fresnel_color.w;
        let refl_uv = clamp(vec2(screen_uv.x, 1.0 - screen_uv.y) + distort, vec2(0.002), vec2(0.998));
        refl_hdr = max(textureSample(refl_tex, refl_sampler, refl_uv).rgb, vec3(0.0));
    }
    let refl = refl_hdr * water.fresnel_color.rgb;

    // Flver_Water.fpo inst 67-77: Fresnel k = mix(g_FresnelBias, 1, (1 - N.V)^g_FresnelPow) * g_FresnelScale.
    let ndotv = clamp(dot(n, v), 0.0, 1.0);
    let f = pow(1.0 - ndotv, water.fresnel.x);
    let k = mix(water.fresnel.y, 1.0, f) * water.fresnel.z;

    // Flver_Water.fpo inst 79-88: directional specular lobes from c11 / c12 combined with reflection.
    let s0 = pow(saturate(dot(r, water.dir0.xyz)), water.fresnel.w) * water.spec0.rgb;
    let s1 = pow(saturate(dot(r, water.dir1.xyz)), water.fresnel.w) * water.spec1.rgb;
    let deep = water.water_color.rgb;
    var col = mix(deep, refl + s0 + s1, k);

    // Flver_Water.fpo inst 89-101: distance + height fog with luma tint.
    let d = length((view.world_position - pos).xz);
    let dfog = saturate(water.fog_dist.x * (1.0 - exp2(-(d - water.fog_dist.y) * water.fog_dist.z)));
    let hfog = saturate(water.fog_height.x * saturate(water.fog_height.z * (pos.y - water.fog_height.y)));
    let luma = dot(col, LUMA);
    col = mix(col, water.fog_col0.rgb * (1.0 + water.fog_col0.w * (luma - 1.0)), dfog);
    col = mix(col, water.fog_col1.rgb * (1.0 + water.fog_col1.w * (luma - 1.0)), hfog);

    var out: FragmentOutput;
    out.color = vec4(col, 1.0);
    return out;
}
