#import bevy_ui::ui_vertex_output::UiVertexOutput

@group(1) @binding(0) var<uniform> tint: vec4<f32>;
// Texel rect u0 v0 u1 v1 inside the font sheet.
@group(1) @binding(1) var<uniform> rect: vec4<f32>;
@group(1) @binding(2) var image: texture_2d<f32>;
@group(1) @binding(3) var image_sampler: sampler;

// Sharp-bilinear sampling for 720p CCM bitmap font sheets:
// - At 1:1 (fw >= 1.0), collapses to standard 1:1 texel sampling.
// - When upscaled (1080p/1440p/4K), keeps texel interiors crisp and restricts
//   linear blending to a 1-screen-pixel anti-aliased band at texel edges.
// - Un-premultiplies RGB on translucent edges to eliminate the dark fringe
//   from empty transparent BC3 blocks (RGB=0, A=0) while preserving opaque
//   shaded icons (e.g. controller buttons).
@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let tex_size = vec2<f32>(textureDimensions(image));
    let texel = mix(rect.xy, rect.zw, in.uv);

    let nearest = floor(texel + 0.5);
    let dist = texel - nearest;
    let fw = max(fwidth(texel), vec2<f32>(1e-4));
    let sharp_texel = nearest + clamp(dist / fw, vec2<f32>(-0.5), vec2<f32>(0.5));
    let clamped = clamp(sharp_texel, rect.xy + 0.5, rect.zw - 0.5);
    let uv = clamped / tex_size;

    let sample = textureSample(image, image_sampler, uv);
    let rgb = select(
        sample.rgb,
        min(sample.rgb / max(sample.a, 1e-4), vec3<f32>(1.0)),
        sample.a > 0.0 && sample.a < 0.99,
    );

    return vec4<f32>(rgb, sample.a) * tint;
}
