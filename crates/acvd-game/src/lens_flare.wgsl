// Additive screen-space lens flare, anamorphic streak (`lf09`), and dirty-lens dust (`lfdust`)
// sprite shader (`model/filter/lensflare/lensflare_t.bnd|lensflare.tpf`, 360 `0x82c9f5b0`,
// `0x82ca0370`, `0x82ca1510`). All 12 TPF entries are DXT1 (black background, no alpha channel),
// so RGB is premultiplied by tint alpha and blended additively (`One + One`).
#import bevy_ui::ui_vertex_output::UiVertexOutput

@group(1) @binding(0) var<uniform> tint: vec4<f32>;
@group(1) @binding(1) var image: texture_2d<f32>;
@group(1) @binding(2) var image_sampler: sampler;

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let tex = textureSample(image, image_sampler, in.uv);
    let rgb = tex.rgb * tint.rgb * tint.a;
    return vec4<f32>(rgb, 1.0);
}
