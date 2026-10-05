#import bevy_ui::ui_vertex_output::UiVertexOutput

@group(1) @binding(0) var<uniform> tint: vec4<f32>;
// Texel rect u0 v0 u1 v1 (flips already swapped in).
@group(1) @binding(1) var<uniform> rect: vec4<f32>;
@group(1) @binding(2) var image: texture_2d<f32>;
@group(1) @binding(3) var image_sampler: sampler;

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let uv = mix(rect.xy, rect.zw, in.uv) / vec2<f32>(textureDimensions(image));
    return textureSample(image, image_sampler, uv) * tint;
}
