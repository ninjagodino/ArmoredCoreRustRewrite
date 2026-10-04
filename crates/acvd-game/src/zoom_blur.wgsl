// Speed blur (`shader/filter_shader.bnd` ZoomBlur_ZoomBlurNoPS / ZoomBlur_ZoomBlurHiPS, drawn by
// 360 0x82c98368 with its +0x38 flag set): the centre rectangle is copied, and the frame around
// it takes 10 taps toward the screen centre, weighted by a ramp that is 0 on the rectangle and
// `thin` on the screen edge (per-vertex z of the four frame quads), times the distance from centre.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct ZoomBlur {
    step: f32,
    alpha: f32,
    thin: f32,
    no_effect: vec2<f32>,
}

@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var screen_sampler: sampler;
@group(0) @binding(2) var<uniform> blur: ZoomBlur;

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let centre = textureSampleLevel(screen, screen_sampler, in.uv, 0.0);
    let d = in.uv * 2.0 - 1.0;
    let edge = (abs(d) - blur.no_effect) / max(1.0 - blur.no_effect, vec2(1e-4));
    let ramp = max(edge.x, edge.y);
    if blur.alpha <= 0.0 || ramp <= 0.0 {
        return centre;
    }
    let w = saturate(blur.thin * ramp * length(d));
    let step = d * blur.step * w;
    var sum = centre.rgb;
    for (var k = 1; k < 10; k++) {
        sum += textureSampleLevel(screen, screen_sampler, in.uv - step * f32(k), 0.0).rgb;
    }
    return vec4(mix(centre.rgb, sum * 0.1, blur.alpha), centre.a);
}
