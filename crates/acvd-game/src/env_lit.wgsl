// Map & garage world pieces lit like the 360 Flver_ColDif / Flver_ColDifMul pixel shaders
// (`sheets/map_env.csv`, ps.*, ps.multi, and dynamic `LightAcc` point/spot lights):
// single or two-texture (UV0/UV1 blended by 1 - vcol.a) squared albedo, two directional lights
// plus a hemisphere ambient from the piece's light set, optional Bevy clustered PointLight +
// SpotLight (with shadows, matching 360 deferred `LightAcc` in garage workshops), optional
// emissive glow (`Map_Diffuse _Grow` / `Ene_Diffuse_Glow`), and distance + height fog whose
// colours follow the lit colour's luminance. Outputs linear HDR; `m_ToneMapFilter` (`0x82c18628`,
// `c31 = 0.5`) runs once in the fullscreen post-process pass (`zoom_blur.wgsl`) after HDR Bloom.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, calculate_F0_dielectric},
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings as view_bindings,
    mesh_view_types,
    lighting,
    lighting::LAYER_BASE,
    clustered_forward as clustering,
    shadows,
    mesh_types::MESH_FLAGS_SHADOW_RECEIVER_BIT,
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
    /// `x = has_diffuse1`, `y = has_diffuse2` (`Map_Diffuse_Multi`),
    /// `z = dynamic_lights` (`LightAcc` clustered point + spot lights),
    /// `w = emissive_boost` (`Map_Diffuse _Grow` / `Ene_Diffuse_Glow`).
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
        // When alpha-blended / alpha-tested (e.g. garage decals, fences), keep RGB un-premultiplied
        // by tex1.a so translucent edges don't double-darken; on opaque 360 textures `tex.rgb * tex.a`
        // matches Flver_ColDif (`sheets/map_env.csv` ps.albedo).
        let a_mul = select(s1.a, 1.0, base.a < 0.999);
        t1 = srgb_encode(s1.rgb) * a_mul;
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

    // Optional deferred-equivalent `LightAcc` pass (`0x82be8218`): evaluate Bevy clustered
    // PointLights and shadow-casting SpotLights on top of the piece's own light set (`SET_MAP`).
    if env.flags.z > 0.5 {
        let NdotV = max(dot(n, pbr.V), 0.0001);
        let R = reflect(-pbr.V, n);
        let perceptual_roughness = pbr.material.perceptual_roughness;
        let roughness = lighting::perceptualRoughnessToRoughness(perceptual_roughness);
        let F_ab = lighting::F_AB(perceptual_roughness, NdotV);

        var lighting_input: lighting::LightingInput;
        lighting_input.layers[LAYER_BASE].NdotV = NdotV;
        lighting_input.layers[LAYER_BASE].N = n;
        lighting_input.layers[LAYER_BASE].R = R;
        lighting_input.layers[LAYER_BASE].perceptual_roughness = perceptual_roughness;
        lighting_input.layers[LAYER_BASE].roughness = roughness;
        lighting_input.P = pbr.world_position.xyz;
        lighting_input.V = pbr.V;
        lighting_input.diffuse_color = albedo;
        lighting_input.metallic = pbr.material.metallic;
        lighting_input.F0_dielectric = calculate_F0_dielectric(pbr.material.reflectance);
        lighting_input.F0_metallic = albedo;
        lighting_input.F_ab = F_ab;

        let view_z = dot(vec4<f32>(
            view_bindings::view.view_from_world[0].z,
            view_bindings::view.view_from_world[1].z,
            view_bindings::view.view_from_world[2].z,
            view_bindings::view.view_from_world[3].z
        ), pbr.world_position);
        let cluster_index = clustering::view_fragment_cluster_index(
            pbr.frag_coord.xy,
            view_z,
            pbr.is_orthographic,
        );
        let ranges = clustering::unpack_clusterable_object_index_ranges(cluster_index);

        var dyn_light = vec3<f32>(0.0);
        for (var i: u32 = ranges.first_point_light_index_offset;
                i < ranges.first_spot_light_index_offset;
                i = i + 1u) {
            let light_id = clustering::get_clusterable_object_id(i);
            var shadow: f32 = 1.0;
            if ((pbr.flags & MESH_FLAGS_SHADOW_RECEIVER_BIT) != 0u
                    && (view_bindings::clustered_lights.data[light_id].flags
                        & mesh_view_types::POINT_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u) {
                shadow = shadows::fetch_point_shadow(
                    light_id,
                    pbr.world_position,
                    pbr.world_normal,
                    pbr.frag_coord.xy,
                );
            }
            dyn_light += lighting::point_light(light_id, &lighting_input, true, true) * shadow;
        }
        for (var i: u32 = ranges.first_spot_light_index_offset;
                i < ranges.first_reflection_probe_index_offset;
                i = i + 1u) {
            let light_id = clustering::get_clusterable_object_id(i);
            var shadow: f32 = 1.0;
            if ((pbr.flags & MESH_FLAGS_SHADOW_RECEIVER_BIT) != 0u
                    && (view_bindings::clustered_lights.data[light_id].flags
                        & mesh_view_types::POINT_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u) {
                shadow = shadows::fetch_spot_shadow(
                    light_id,
                    pbr.world_position,
                    pbr.world_normal,
                    view_bindings::clustered_lights.data[light_id].shadow_map_near_z,
                    pbr.frag_coord.xy,
                );
            }
            dyn_light += lighting::spot_light(light_id, &lighting_input, true) * shadow;
        }
        col += dyn_light * view_bindings::view.exposure;
    }

    // Emissive fixture boost (`Map_Diffuse _Grow` / `Ene_Diffuse_Glow` / `Carlight_Phong`).
    if env.flags.w > 0.001 {
        col += albedo * env.flags.w;
    }

    let pos = in.world_position.xyz;
    let d = length((view_bindings::view.world_position - pos).xz);
    let dfog = saturate(env.fog_dist.x * (1.0 - exp2(-(d - env.fog_dist.y) * env.fog_dist.z)));
    let hfog = saturate(env.fog_height.x * saturate(env.fog_height.z * (pos.y - env.fog_height.y)));
    let luma = dot(col, LUMA);
    col = mix(col, env.fog_col0.rgb * (1.0 + env.fog_col0.w * (luma - 1.0)), dfog);
    col = mix(col, env.fog_col1.rgb * (1.0 + env.fog_col1.w * (luma - 1.0)), hfog);

    var out: FragmentOutput;
    out.color = vec4(col, base.a);
    return out;
}
