// Action-43 heat haze / screen-space distortion (`Sfx_DistortionType1`, `0x835b9580` /
// `0x835b9768`). The 360 pixel shader samples the scene at the projected quad UV offset by
// `normal.xy * c7 * max(0, 1 - r * c6.y)`, with the normal map fetched at `uv + (c9,c10)*c6`.
// The DXN normal map is packed into the pass uniform as a 32x32 table.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct HazeQuad {
    center: vec2<f32>,
    axis_u: vec2<f32>,
    axis_v: vec2<f32>,
    depth: f32,
    strength: f32,
    power: f32,
    normal_scale: vec2<f32>,
    color: vec4<f32>,
}

const MAX_HAZE: u32 = 16u;
const NORMAL_N: u32 = 32u;

struct HazePass {
    quads: array<HazeQuad, 16>,
    count: u32,
    flags: u32,
    normals: array<vec4<f32>, 1024>,
}

@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var screen_sampler: sampler;
@group(0) @binding(2) var<uniform> haze: HazePass;

fn normal_at(uv: vec2<f32>) -> vec2<f32> {
    let p = fract(uv) * f32(NORMAL_N);
    let i = vec2<u32>(floor(p));
    let f = fract(p);
    let x0 = i.x % NORMAL_N;
    let y0 = i.y % NORMAL_N;
    let x1 = (x0 + 1u) % NORMAL_N;
    let y1 = (y0 + 1u) % NORMAL_N;
    let n00 = haze.normals[y0 * NORMAL_N + x0].xy;
    let n10 = haze.normals[y0 * NORMAL_N + x1].xy;
    let n01 = haze.normals[y1 * NORMAL_N + x0].xy;
    let n11 = haze.normals[y1 * NORMAL_N + x1].xy;
    let n = mix(mix(n00, n10, f.x), mix(n01, n11, f.x), f.y);
    return n * 2.0 - 1.0;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let base = textureSampleLevel(screen, screen_sampler, in.uv, 0.0);
    if haze.count == 0u {
        return base;
    }
    var out = base;
    for (var qi = 0u; qi < MAX_HAZE; qi++) {
        if qi >= haze.count {
            break;
        }
        let h = haze.quads[qi];
        let rel = in.uv - h.center;
        let det = h.axis_u.x * h.axis_v.y - h.axis_u.y * h.axis_v.x;
        if abs(det) < 1e-7 {
            continue;
        }
        let u = (rel.x * h.axis_v.y - rel.y * h.axis_v.x) / det;
        let v = (h.axis_u.x * rel.y - h.axis_u.y * rel.x) / det;
        if abs(u) > 1.0 || abs(v) > 1.0 {
            continue;
        }
        let local = vec2<f32>(u, v);
        // Slots 9/10 phase-shift the normal-map lookup; slot 6 scales the normal sampling
        // in the 360 shader, but the table is already one normal cell per quad.
        // `mad r0.w, -c10.x, c6.x, uv.y` / `mad r0.z, c9.x, c6.x, uv.x`: the normal lookup
        // advances +c9 and -c10 in quad UV, scaled by c6.x.
        let nuv = local * 0.5 + vec2<f32>(0.5)
            + h.normal_scale * vec2<f32>(1.0, -1.0) * h.power;
        let n = normal_at(nuv);
        let r = length(local);
        let falloff = clamp(1.0 - r * h.power, 0.0, 1.0);
        // `mad r0.xy, normal * c7 * falloff, c255.w, screen_uv`: c255.w is 1, and the quad
        // local offset is in projected quad units, so map the normal through the half-axes.
        let offset = (n.x * h.axis_u + n.y * h.axis_v) * h.strength * falloff;
        let scene = textureSampleLevel(
            screen,
            screen_sampler,
            clamp(in.uv + offset, vec2<f32>(0.002), vec2<f32>(0.998)),
            0.0,
        );
        out = vec4<f32>(mix(out.rgb, scene.rgb * h.color.rgb, h.color.a), out.a);
    }
    return out;
}
