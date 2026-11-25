// Additive screen-space lens-flare sprite shader (`model/filter/lensflare/lensflare_t.bnd|lensflare.tpf`,
// 360 `0x82c9f5b0`, `0x82ca0370`, `0x82ca1510`). All 12 TPF entries are DXT1 (black background,
// no alpha channel), so RGB is premultiplied by the tint alpha and blended additively
// (`One + One`). The quads are drawn in the 3D transparent phase, so the camera's HDR `Bloom`
// and `m_ToneMapFilter` tonemap them exactly like the 360's HDR sprite batch (`0x82b95c28`).
#import bevy_pbr::forward_io::VertexOutput

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> tint: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var image: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var image_sampler: sampler;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex = textureSample(image, image_sampler, in.uv);
    let rgb = tex.rgb * tint.rgb * tint.a;
    // `AlphaMode::Add` premultiplies by alpha and zeroes alpha in the stock PBR shader; do the
    // same here (rgb already includes `tint.a`) so the blend is plain `src + dst`.
    return vec4<f32>(rgb, 0.0);
}
