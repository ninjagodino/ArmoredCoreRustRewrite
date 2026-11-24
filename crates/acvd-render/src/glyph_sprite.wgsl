#import bevy_ui::ui_vertex_output::UiVertexOutput

@group(1) @binding(0) var<uniform> tint: vec4<f32>;
// Texel rect u0 v0 u1 v1 inside the font sheet.
@group(1) @binding(1) var<uniform> rect: vec4<f32>;
@group(1) @binding(2) var image: texture_2d<f32>;
@group(1) @binding(3) var image_sampler: sampler;

// Catmull-Rom cubic basis weights for fractional offset `f` in [0, 1).
// Interpolating (w(0) = 1, w(±1) = w(±2) = 0): reproduces exact original texels
// at 1:1 texel centers with zero blur, and reconstructs smooth C1-continuous
// anti-aliased glyph curves when upscaled (1080p/1440p/4K) without the blocky
// stair-stepping of sharp-bilinear or the haze of plain bilinear.
fn catmull_rom(f: f32) -> vec4<f32> {
    let f2 = f * f;
    let f3 = f2 * f;
    return vec4<f32>(
        -0.5 * f3 +       f2 - 0.5 * f,
         1.5 * f3 - 2.5 * f2 + 1.0,
        -1.5 * f3 + 2.0 * f2 + 0.5 * f,
         0.5 * f3 - 0.5 * f2,
    );
}

// Fetches one texel as premultiplied RGBA, treating texels outside the glyph's
// atlas rect as transparent so neighboring glyphs never bleed into the 4x4 kernel.
fn fetch_premul(coord: vec2<i32>, rmin: vec2<i32>, rmax: vec2<i32>) -> vec4<f32> {
    if any(coord < rmin) || any(coord >= rmax) {
        return vec4<f32>(0.0);
    }
    let s = textureLoad(image, coord, 0);
    return vec4<f32>(s.rgb * s.a, s.a);
}

fn sample_row(base_x: i32, y: i32, wx: vec4<f32>, rmin: vec2<i32>, rmax: vec2<i32>) -> vec4<f32> {
    return fetch_premul(vec2<i32>(base_x - 1, y), rmin, rmax) * wx.x
         + fetch_premul(vec2<i32>(base_x,     y), rmin, rmax) * wx.y
         + fetch_premul(vec2<i32>(base_x + 1, y), rmin, rmax) * wx.z
         + fetch_premul(vec2<i32>(base_x + 2, y), rmin, rmax) * wx.w;
}

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(image));
    let tex_size = vec2<f32>(dims);
    let texel = mix(rect.xy, rect.zw, in.uv);
    let fw = max(fwidth(texel), vec2<f32>(1e-4));

    // Hardware bilinear sample (executed in uniform control flow for derivatives).
    let clamped_uv = clamp(texel, rect.xy + 0.5, rect.zw - 0.5) / tex_size;
    let linear_s = textureSample(image, image_sampler, clamped_uv);
    let linear_rgb = select(
        linear_s.rgb,
        min(linear_s.rgb / max(linear_s.a, 1e-4), vec3<f32>(1.0)),
        linear_s.a > 0.0 && linear_s.a < 0.99,
    );

    // When downscaling, hardware linear filtering avoids point-sampling aliasing.
    if max(fw.x, fw.y) > 1.05 {
        return vec4<f32>(linear_rgb, linear_s.a) * tint;
    }

    // 4x4 Catmull-Rom bicubic in premultiplied space:
    // - Eliminates dark fringes from empty BC3 blocks (RGB=0, A=0) identically
    //   for both monochrome text and colored controller-button icons.
    // - Bounds every tap to [rect.xy, rect.zw) so adjacent glyphs never bleed.
    let rmin = max(vec2<i32>(floor(rect.xy + 0.001)), vec2<i32>(0));
    let rmax = min(vec2<i32>(ceil(rect.zw - 0.001)), dims);
    let p = texel - 0.5;
    let base = vec2<i32>(floor(p));
    let f = p - vec2<f32>(base);
    let wx = catmull_rom(f.x);
    let wy = catmull_rom(f.y);

    let acc = sample_row(base.x, base.y - 1, wx, rmin, rmax) * wy.x
            + sample_row(base.x, base.y,     wx, rmin, rmax) * wy.y
            + sample_row(base.x, base.y + 1, wx, rmin, rmax) * wy.z
            + sample_row(base.x, base.y + 2, wx, rmin, rmax) * wy.w;

    let a = clamp(acc.a, 0.0, 1.0);
    let rgb = select(vec3<f32>(0.0), clamp(acc.rgb / a, vec3<f32>(0.0), vec3<f32>(1.0)), a > 1e-4);
    return vec4<f32>(rgb, a) * tint;
}
