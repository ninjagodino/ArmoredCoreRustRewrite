// Map_Sky / Map_SkyMulti / Map_NoLightNoFog (`NoLightNoFog.spx`): squared texel, unlit and
// unfogged, written as linear HDR so HDR Bloom and fullscreen `m_ToneMapFilter` (`0x82c18628`,
// `zoom_blur.wgsl`) tonemap the sky identically to the 360. When `flags.x > 0.5` (sortie sky
// domes `o9700..o9705`), writes depth = 0.0 (far plane in reverse Z, matching 360 `0x827f3d38`).

#import bevy_pbr::forward_io::VertexOutput

/// `x > 0.5`: force far-plane depth (0.0 in reverse Z); `y > 0.5`: use texture alpha for blend;
/// `z`: linear HDR scale (default 1.0).
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> sky_flags: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var sky_tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var sky_sampler: sampler;

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
fn fragment(in: VertexOutput) -> SkyOut {
    // Sample sky_tex directly so dome vertex alpha does not zero out tex.a near the horizon.
    let s = textureSample(sky_tex, sky_sampler, in.uv);
    let scale = max(sky_flags.z, 0.001);
    var out: SkyOut;
    if sky_flags.y > 0.5 {
        // Alpha-blended cloud dome (`Map_SkyMulti`) or additive sky (`Map_NoLightNoFog_Add`).
        let raw = srgb_encode(s.rgb);
        out.color = vec4(raw * raw * scale, s.a);
    } else {
        let raw = srgb_encode(s.rgb) * s.a;
        out.color = vec4(raw * raw * scale, 1.0);
    }
    // Reverse Z: 0.0 is the far plane; otherwise keep normal rasterized depth.
    out.depth = select(in.position.z, 0.0, sky_flags.x > 0.5);
    return out;
}
