// Fullscreen post-processing pass:
// 1. Speed zoom blur (`shader/filter_shader.bnd` ZoomBlur_ZoomBlurNoPS / ZoomBlur_ZoomBlurHiPS,
//    drawn by 360 `0x82c98368`).
// 2. Crepuscular god rays (`LensFlareGodRay12/16/20PS` + `LensFlareGodRayMerge*PS`, 360
//    `0x82c9f9f0`).
// 3. `m_ToneMapFilter` (`shader/filter_shader.bnd` `ToneMap_{Exp,Divide,Reinhard}ToneMap*PS`,
//    360 `0x82c18628`): `c31 = 0.5` halving + tonemapping applied once after HDR Bloom to the
//    entire composited 3D frame (terrain, garage, sky dome, water, AC parts, lights, VFX).
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct GodRayFilter {
    // Sun screen position in [0, 1] UV space (`c28.zw` in 0x82c9fec8..0x82c9fee4).
    sun_uv: vec2<f32>,
    // Radial blur constants (`c29` in 0x82c9fe84..0x82c9fefc): density, decay, weight, exposure.
    density: f32,
    decay: f32,
    weight: f32,
    exposure: f32,
    // `merge_intensity * visibility` (`c28.x` in 0x82ca00c0); <= 0 disables the pass.
    intensity: f32,
    // Emitter radial envelope radius in UV space (from `emitter_scale`).
    emitter_radius: f32,
    // `xyz = merge_color.rgb / 255`, `w = merge_mode` (0 Add, 1 Modulate, 2 Transparent).
    merge_color: vec4<f32>,
    // `xyz = emitter_color.rgb / 255`, `w = sample_count` (12, 16, or 20).
    emitter_color: vec4<f32>,
}

struct ZoomBlur {
    step: f32,
    alpha: f32,
    thin: f32,
    no_effect: vec2<f32>,
    god_ray: GodRayFilter,
    // `m_ToneMapFilter` (`0x82c18628`): `(c32 = 1 / (lum * key), c33 = mul, c34 = white, mode)`.
    tonemap: vec4<f32>,
    // Garage menu blurred bokeh + `+` reticle grid backdrop:
    // `(blur_strength, dim_factor, grid_alpha, grid_spacing_720p)`.
    menu_backdrop: vec4<f32>,
    // Player display tone adjustments: `(gamma, brightness, contrast, 0.0)`.
    display_adjust: vec4<f32>,
}

@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var screen_sampler: sampler;
@group(0) @binding(2) var<uniform> blur: ZoomBlur;

fn apply_god_rays(base: vec3<f32>, uv_in: vec2<f32>, gr: GodRayFilter) -> vec3<f32> {
    if gr.intensity <= 0.0001 || gr.density <= 0.0001 || gr.emitter_color.w < 1.0 {
        return base;
    }

    let samples = clamp(i32(gr.emitter_color.w + 0.5), 12, 20);
    let delta = (uv_in - gr.sun_uv) * (gr.density / f32(samples));
    let inv_rad_sq = 1.0 / max(gr.emitter_radius * gr.emitter_radius, 0.0025);

    var uv = uv_in;
    var illum_decay = 1.0;
    var accum = vec3<f32>(0.0);

    for (var i = 0; i < 20; i++) {
        if i >= samples {
            break;
        }
        uv -= delta;
        let suv = clamp(uv, vec2<f32>(0.001), vec2<f32>(0.999));
        let col = textureSampleLevel(screen, screen_sampler, suv, 0.0).rgb;

        // Isolate bright sky / sun disc pixels so silhouetted terrain, buildings, and the AC
        // carve dark crepuscular shadow shafts along the radial blur.
        let luma = dot(col, vec3<f32>(0.29891, 0.58661, 0.11448));
        let sky_mask = smoothstep(0.32, 0.88, luma);

        let d = suv - gr.sun_uv;
        let radial = exp(-dot(d, d) * inv_rad_sq);
        let emitter = col * sky_mask * (0.25 + 0.75 * radial);

        accum += emitter * illum_decay * gr.weight;
        illum_decay *= gr.decay;
    }

    let norm = accum / f32(samples);
    let tint = max(gr.emitter_color.rgb * 4.0, vec3<f32>(0.25)) * gr.merge_color.rgb;
    let rays = norm * gr.exposure * gr.intensity * tint;

    let mode = i32(gr.merge_color.w + 0.5);
    if mode == 1 {
        // LensFlareGodRayMergeModulatePS
        return base * (vec3<f32>(1.0) + rays);
    } else if mode == 2 {
        // LensFlareGodRayMergeTransparentPS
        return base + rays * (vec3<f32>(1.0) - saturate(base) * 0.35);
    } else {
        // LensFlareGodRayMergeAddPS (mode 0)
        return base + rays;
    }
}

// Exact IEC sRGB -> linear decode: Xbox 360 `m_ToneMapFilter` (`0x82c18628`) outputs
// display-referred sRGB directly to an 8-bit A8R8G8B8 framebuffer (no hardware sRGB write),
// whereas Bevy's fullscreen post-process writes to a linear HDR target before the sRGB swapchain
// applies hardware linear -> sRGB encoding. Decoding here cancels the swapchain encode so the
// final display pixels match the 360 framebuffer 1:1.
fn srgb_decode(c: vec3<f32>) -> vec3<f32> {
    let clamped = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = clamped / 12.92;
    let hi = pow((clamped + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, clamped <= vec3<f32>(0.04045));
}

fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
    let clamped = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = clamped * 12.92;
    let hi = 1.055 * pow(clamped, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, clamped <= vec3<f32>(0.0031308));
}

fn apply_display_adjust(linear_rgb: vec3<f32>, adj: vec4<f32>) -> vec3<f32> {
    let gamma = max(adj.x, 0.1);
    let brightness = adj.y;
    let contrast = adj.z;
    if abs(gamma - 1.0) < 0.0001
        && abs(brightness - 1.0) < 0.0001
        && abs(contrast - 1.0) < 0.0001
    {
        return linear_rgb;
    }
    var d = srgb_encode(linear_rgb);
    // Contrast pivots around mid-gray (0.5); brightness shifts display luminance cleanly.
    d = clamp(
        (d - vec3<f32>(0.5)) * contrast + vec3<f32>(0.5) + vec3<f32>((brightness - 1.0) * 0.35),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    // Higher gamma (> 1.0) lifts midtones brighter; lower gamma (< 1.0) darkens midtones.
    d = pow(max(d, vec3<f32>(0.0)), vec3<f32>(1.0 / gamma));
    return srgb_decode(d);
}

fn apply_tonemap(hdr: vec3<f32>, tm: vec4<f32>) -> vec3<f32> {
    if tm.w > 10.0 || tm.x <= 0.0 {
        return hdr;
    }
    let src = max(hdr, vec3<f32>(0.0)) * 0.5;
    let x = src * tm.x;
    let mode = u32(tm.w + 0.5);
    var display = src;
    if mode == 0u {
        display = sqrt(max(vec3<f32>(1.0) - exp2(-x), vec3<f32>(0.0))) * tm.y;
    } else if mode == 1u {
        display = (x / (x + vec3<f32>(1.0))) * tm.y;
    } else if mode == 2u {
        let w = max(tm.z * tm.x, 1e-4);
        let w2 = w * w;
        display = (x * (vec3<f32>(1.0) + x / w2) / (x + vec3<f32>(1.0))) * tm.y;
    }
    return srgb_decode(display);
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let centre = textureSampleLevel(screen, screen_sampler, in.uv, 0.0);
    var rgb = centre.rgb;

    // 1. Speed zoom-blur (`0x82c98368`).
    let d = in.uv * 2.0 - 1.0;
    let edge = (abs(d) - blur.no_effect) / max(1.0 - blur.no_effect, vec2(1e-4));
    let ramp = max(edge.x, edge.y);
    if blur.alpha > 0.0 && ramp > 0.0 {
        let w = saturate(blur.thin * ramp * length(d));
        let step = d * blur.step * w;
        var sum = rgb;
        for (var k = 1; k < 10; k++) {
            sum += textureSampleLevel(screen, screen_sampler, in.uv - step * f32(k), 0.0).rgb;
        }
        rgb = mix(rgb, sum * 0.1, blur.alpha);
    }

    // 2. Crepuscular god-ray radial blur + merge (`0x82c9f9f0`).
    rgb = apply_god_rays(rgb, in.uv, blur.god_ray);

    // 2b. Garage menu out-of-focus bokeh blur (24-tap Vogel disk on HDR scene before tonemap).
    let menu_blur = blur.menu_backdrop.x;
    if menu_blur > 0.001 {
        let dim = vec2<f32>(textureDimensions(screen));
        let ui_scale = max(dim.y / 720.0, 0.5);
        let radius_px = 16.0 * ui_scale * menu_blur;
        let inv_dim = 1.0 / max(dim, vec2<f32>(1.0));
        var accum = vec3<f32>(0.0);
        let samples = 24;
        for (var i = 0; i < 24; i++) {
            let fi = f32(i);
            let r = sqrt((fi + 0.5) / f32(samples)) * radius_px;
            let theta = fi * 2.39996323;
            let off = vec2<f32>(cos(theta), sin(theta)) * r * inv_dim;
            let suv = clamp(in.uv + off, vec2<f32>(0.001), vec2<f32>(0.999));
            accum += textureSampleLevel(screen, screen_sampler, suv, 0.0).rgb;
        }
        rgb = mix(rgb, accum / f32(samples), menu_blur);
    }

    // 3. `m_ToneMapFilter` (`0x82c18628`, `c31 = 0.5` + Divide/Reinhard/Exp tonemap).
    rgb = apply_tonemap(rgb, blur.tonemap);

    // 4. Garage menu dark slate backdrop tint + crisp `+` reticle grid overlay.
    if menu_blur > 0.001 {
        let dim_factor = blur.menu_backdrop.y;
        rgb = rgb * dim_factor + vec3<f32>(0.006, 0.011, 0.020) * menu_blur;

        let grid_alpha = blur.menu_backdrop.z;
        if grid_alpha > 0.001 {
            let dim = vec2<f32>(textureDimensions(screen));
            let ui_scale = max(dim.y / 720.0, 0.5);
            let spacing = max(blur.menu_backdrop.w * ui_scale, 16.0);
            let px = in.uv * dim;
            let cell = abs(fract((px + spacing * 0.5) / spacing) - 0.5) * spacing;
            let arm = 4.5 * ui_scale;
            let half_thick = max(0.65 * ui_scale, 0.55);
            let horiz = step(cell.y, half_thick) * step(cell.x, arm);
            let vert = step(cell.x, half_thick) * step(cell.y, arm);
            let cross = max(horiz, vert);
            let cross_col = vec3<f32>(0.62, 0.70, 0.78);
            rgb = mix(rgb, cross_col, cross * grid_alpha);
        }
    }

    // 5. Player display adjustments (Gamma, Brightness, Contrast from Options -> Graphics).
    rgb = apply_display_adjust(rgb, blur.display_adjust);

    return vec4<f32>(rgb, centre.a);
}
