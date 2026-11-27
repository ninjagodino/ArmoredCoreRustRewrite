#import bevy_ui::ui_vertex_output::UiVertexOutput

@group(1) @binding(0) var<uniform> tint: vec4<f32>;
// Texel rects u0 v0 u1 v1 (flips already swapped in).
@group(1) @binding(1) var<uniform> rect: vec4<f32>;
@group(1) @binding(2) var<uniform> mask_rect: vec4<f32>;
// x: 1 - fill.
@group(1) @binding(3) var<uniform> threshold: vec4<f32>;
@group(1) @binding(4) var image: texture_2d<f32>;
@group(1) @binding(5) var image_sampler: sampler;
@group(1) @binding(6) var mask: texture_2d<f32>;
@group(1) @binding(7) var mask_sampler: sampler;

// Sprite_AlphaRef.fpo: kill where mask alpha - c0.x < 0, else texture times vertex color.
@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let sprite_texel = mix(rect.xy, rect.zw, in.uv);
    let uv = sprite_texel / vec2<f32>(textureDimensions(image));
    // The 360 fetches the mask at the same interpolated texcoord as the sprite
    // (`tfetch2D r0.__w_, r1.xy, tf1`). A zero mask rect is how the DRB marks "use the
    // sprite's own normalized UVs" (all `ACV_OutLine_LockCursor_*` CursorLines author
    // `mask_uv` 0 and rely on this), so do not collapse it to texel (0,0).
    let mask_span = mask_rect.zw - mask_rect.xy;
    var muv = uv;
    if any(mask_span != vec2<f32>(0.0)) {
        muv = mix(mask_rect.xy, mask_rect.zw, in.uv) / vec2<f32>(textureDimensions(mask));
    }
    let color = textureSample(image, image_sampler, uv) * tint;
    if textureSample(mask, mask_sampler, muv).a < threshold.x {
        discard;
    }
    return color;
}
