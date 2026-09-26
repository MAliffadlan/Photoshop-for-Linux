struct Parameters {
    canvas: vec4<f32>, // Output width/height, document width/height.
    bounds: vec4<f32>, // Origin and extent.
    rotation: vec4<f32>, // Cosine, sine, horizontal and vertical signs.
    flags: vec4<u32>, // Blend mode, adjustment kind, coverage present, curve point count.
    appearance: vec4<f32>, // Opacity, motion blur samples, motion vector in source UV.
    first: vec4<f32>,
    second: vec4<f32>,
    points: array<vec4<f32>, 128>,
}

fn lum(c: vec3<f32>) -> f32 { return dot(c, vec3(0.3, 0.59, 0.11)); }
fn sat(c: vec3<f32>) -> f32 { return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b); }

fn set_lum(color: vec3<f32>, value: f32) -> vec3<f32> {
    var c = color + value - lum(color);
    let low = min(min(c.r, c.g), c.b);
    let high = max(max(c.r, c.g), c.b);
    if low < 0.0 { c = value + (c - value) * value / max(value - low, 0.000001); }
    if high > 1.0 { c = value + (c - value) * (1.0 - value) / max(high - value, 0.000001); }
    return c;
}

fn set_sat(c: vec3<f32>, value: f32) -> vec3<f32> {
    let low = min(min(c.r, c.g), c.b);
    let high = max(max(c.r, c.g), c.b);
    return select(vec3(0.0), (c - low) * value / max(high - low, 0.000001), high > low);
}

fn color_dodge(d: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return select(min(vec3(1.0), d / max(1.0 - s, vec3(0.000001))), vec3(0.0), d <= vec3(0.0));
}

fn color_burn(d: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return select(1.0 - min(vec3(1.0), (1.0 - d) / max(s, vec3(0.000001))), vec3(1.0), d >= vec3(1.0));
}

/// Burns or dodges according to the source, with twice its distance from mid grey.
fn vivid_light(d: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    return select(color_dodge(d, 2.0 * s - 1.0), color_burn(d, 2.0 * s), s <= vec3(0.5));
}

// Mode numbers are the positions in BlendMode::ALL, so the ten modes Compositor 1.2.2 added
// renumber the cases after them. Names and formulas follow Photoshop there.
fn blend(d: vec3<f32>, s: vec3<f32>, mode: u32) -> vec3<f32> {
    switch mode {
        case 1u: { return min(d, s); }
        case 2u: { return d * s; }
        case 3u: { return color_burn(d, s); }
        case 4u: { return max(d + s - 1.0, vec3(0.0)); }
        case 5u: { return max(d, s); }
        case 6u: { return d + s - d * s; }
        case 7u: { return color_dodge(d, s); }
        case 8u: { return min(d + s, vec3(1.0)); }
        case 9u: { return select(1.0 - 2.0 * (1.0 - d) * (1.0 - s), 2.0 * d * s, d <= vec3(0.5)); }
        case 10u: {
            let soft = select(sqrt(d), ((16.0 * d - 12.0) * d + 4.0) * d, d <= vec3(0.25));
            return select(d + (2.0 * s - 1.0) * (soft - d), d - (1.0 - 2.0 * s) * d * (1.0 - d), s <= vec3(0.5));
        }
        case 11u: { return select(1.0 - 2.0 * (1.0 - d) * (1.0 - s), 2.0 * d * s, s <= vec3(0.5)); }
        case 12u: { return vivid_light(d, s); }
        case 13u: { return clamp(d + 2.0 * s - 1.0, vec3(0.0), vec3(1.0)); }
        case 14u: { return select(max(d, 2.0 * s - 1.0), min(d, 2.0 * s), s <= vec3(0.5)); }
        case 15u: { return select(vec3(0.0), vec3(1.0), vivid_light(d, s) >= vec3(0.5)); }
        case 16u: { return abs(d - s); }
        case 17u: { return d + s - 2.0 * d * s; }
        case 18u: { return max(d - s, vec3(0.0)); }
        case 19u: { return select(min(vec3(1.0), d / max(s, vec3(0.000001))), vec3(1.0), s <= vec3(0.0)); }
        case 20u: { return set_lum(set_sat(s, sat(d)), lum(d)); }
        case 21u: { return set_lum(set_sat(d, sat(s)), lum(d)); }
        case 22u: { return set_lum(s, lum(d)); }
        case 23u: { return set_lum(d, lum(s)); }
        default: { return s; }
    }
}

fn hue_to_rgb(hue: f32, saturation: f32, lightness: f32) -> vec3<f32> {
    let h = ((hue % 360.0) + 360.0) % 360.0 / 60.0;
    let c = (1.0 - abs(2.0 * lightness - 1.0)) * saturation;
    let x = c * (1.0 - abs(h % 2.0 - 1.0));
    var rgb = vec3(c, 0.0, x);
    if h < 1.0 { rgb = vec3(c, x, 0.0); }
    else if h < 2.0 { rgb = vec3(x, c, 0.0); }
    else if h < 3.0 { rgb = vec3(0.0, c, x); }
    else if h < 4.0 { rgb = vec3(0.0, x, c); }
    else if h < 5.0 { rgb = vec3(x, 0.0, c); }
    return rgb + lightness - c * 0.5;
}

fn curve_slope(index: u32, base: u32) -> f32 {
    let a = params.points[base + index].xy;
    let b = params.points[base + index + 1u].xy;
    return (b.y - a.y) / max(b.x - a.x, 0.000001);
}

fn curve_tangent(index: u32, base: u32, count: u32) -> f32 {
    if index == 0u { return curve_slope(0u, base); }
    if index == count - 1u { return curve_slope(index - 1u, base); }
    let a = curve_slope(index - 1u, base);
    let b = curve_slope(index, base);
    if a * b <= 0.0 { return 0.0; }
    return 2.0 / (1.0 / a + 1.0 / b);
}

fn curve(value: f32, base: u32, count: u32) -> f32 {
    var index = 0u;
    for (var i = 0u; i + 1u < count; i++) { if params.points[base + i].x <= value { index = i; } }
    index = min(index, count - 2u);
    let a = params.points[base + index].xy;
    let b = params.points[base + index + 1u].xy;
    let h = max(b.x - a.x, 0.000001);
    let t = clamp((value - a.x) / h, 0.0, 1.0);
    let t2 = t * t;
    let t3 = t2 * t;
    return clamp((2.0*t3 - 3.0*t2 + 1.0)*a.y + (t3 - 2.0*t2 + t)*h*curve_tangent(index, base, count)
        + (-2.0*t3 + 3.0*t2)*b.y + (t3 - t2)*h*curve_tangent(index + 1u, base, count), 0.0, 1.0);
}

fn noise(point: vec2<u32>, seed: u32) -> f32 {
    var value = point.x * 374761393u + point.y * 668265263u + seed;
    value = (value ^ (value >> 13u)) * 1274126177u;
    value = value ^ (value >> 16u);
    return f32(value) / 4294967295.0 * 2.0 - 1.0;
}

fn mix32(input: u32) -> u32 {
    var value = (input ^ (input >> 16u)) * 0x7feb352du;
    value = (value ^ (value >> 15u)) * 0x846ca68bu;
    return value ^ (value >> 16u);
}

fn lattice(point: vec2<i32>, seed: u32) -> f32 {
    let h = mix32(bitcast<u32>(point.x) * 0x9e3779b1u ^ mix32(bitcast<u32>(point.y) * 0x85ebca77u ^ seed));
    return f32(h & 65535u) / 65535.0 + f32(h >> 16u) / 65535.0 - 1.0;
}

// Smooth seeded noise whose features follow the requested size: four
// neighbouring lattice values blended with a smoothstep, scaled by 1.6 because
// blending narrows the spread.
fn grain_field(point: vec2<f32>, size: f32, seed: u32) -> f32 {
    let cell = point / max(size, 0.0001);
    let origin = vec2<i32>(floor(cell));
    let t = fract(cell) * fract(cell) * (3.0 - 2.0 * fract(cell));
    let top = mix(lattice(origin, seed), lattice(origin + vec2(1, 0), seed), t.x);
    let bottom = mix(lattice(origin + vec2(0, 1), seed), lattice(origin + vec2(1, 1), seed), t.x);
    return mix(top, bottom, t.y) * 1.6;
}

fn film_grain(point: vec2<f32>, settings: vec4<f32>) -> f32 {
    let seed = bitcast<u32>(settings.w);
    let coarse = grain_field(point, settings.y, seed);
    // Roughness adds smaller, less regular particles while their size stays
    // proportional to Size, and both scales are smooth, which is what keeps a
    // rough grain from turning into a grid of dots.
    let fine = grain_field(point, max(settings.y * 0.35, 0.5), mix32(seed ^ 0xa511e9b3u));
    return mix(coarse, fine, settings.z / 100.0);
}

fn wrap(value: f32) -> f32 { return ((value % 360.0) + 360.0) % 360.0; }

fn channel_level(value: f32, channel: u32) -> f32 {
    let a = params.points[channel * 2u];
    let white = params.points[channel * 2u + 1u].x;
    let input = clamp((value * 255.0 - a.x) / max(a.z - a.x, 1.0), 0.0, 1.0);
    return (a.w + pow(input, 1.0 / max(a.y, 0.01)) * (white - a.w)) / 255.0;
}

fn adjust_hsv(rgb: vec3<f32>, a: vec4<f32>) -> vec3<f32> {

            let high = max(max(rgb.r, rgb.g), rgb.b);
            let low = min(min(rgb.r, rgb.g), rgb.b);
            var lightness = (high + low) * 0.5;
            let delta = high - low;
            var hue = 0.0;
            var saturation = 0.0;
            if delta > 0.000001 {
                saturation = delta / max(1.0 - abs(2.0 * lightness - 1.0), 0.000001);
                if high == rgb.r { hue = (rgb.g - rgb.b) / delta; }
                else if high == rgb.g { hue = (rgb.b - rgb.r) / delta + 2.0; }
                else { hue = (rgb.r - rgb.g) / delta + 4.0; }
                hue *= 60.0;
            }
            hue += a.x;
            saturation *= 1.0 + a.y / 100.0;
            if a.w > 0.0 { hue = a.x; saturation = a.y / 100.0; }
            let light = clamp(a.z / 100.0, -1.0, 1.0);
            lightness = select(lightness * (1.0 + light), lightness + (1.0 - lightness) * light, light >= 0.0);
            return hue_to_rgb(hue, clamp(saturation, 0.0, 1.0), lightness);
}

/// Black & White, classified by the two strongest channels exactly as Compositor's routine does.
fn adjust_black_white(rgb: vec3<f32>) -> vec3<f32> {
    let w = params.first;
    let mx = max(max(rgb.r, rgb.g), rgb.b);
    let mn = min(min(rgb.r, rgb.g), rgb.b);
    let md = rgb.r + rgb.g + rgb.b - mx - mn;
    var primary = w.x;
    var secondary = select(params.second.y, w.y, rgb.g >= rgb.b);
    if mx == rgb.g {
        primary = w.z;
        secondary = select(w.w, w.y, rgb.r >= rgb.b);
    } else if mx != rgb.r {
        primary = params.second.x;
        secondary = select(params.second.y, w.w, rgb.g >= rgb.r);
    }
    let gray = clamp(mn + (md - mn) * secondary + (mx - md) * primary, 0.0, 1.0);
    let tint = params.second.w;
    if tint <= 0.0 { return vec3(gray); }
    return hue_to_rgb(params.second.z, tint, gray);
}

fn tonal_weights(value: f32) -> vec3<f32> {
    let ramp = 0.25;
    let width = 0.333;
    let scale = 0.7;
    let shadow = clamp((value - width) / -ramp + 0.5, 0.0, 1.0) * scale;
    let highlight = clamp((value + width - 1.0) / ramp + 0.5, 0.0, 1.0) * scale;
    let rise = clamp((value - width) / ramp + 0.5, 0.0, 1.0);
    let fall = clamp((value + width - 1.0) / -ramp + 0.5, 0.0, 1.0);
    return vec3(shadow, rise * fall * scale, highlight);
}

fn color_balance_channel(value: f32, shadows: f32, midtones: f32, highlights: f32) -> f32 {
    let weights = tonal_weights(value);
    return clamp(value + shadows * weights.x + midtones * weights.y + highlights * weights.z, 0.0, 1.0);
}

fn adjust_color_balance(rgb: vec3<f32>) -> vec3<f32> {
    let before = dot(rgb, vec3(0.299, 0.587, 0.114));
    let highlights = params.points[0].xyz;
    var balanced = vec3(
        color_balance_channel(rgb.r, params.first.x, params.second.x, highlights.x),
        color_balance_channel(rgb.g, params.first.y, params.second.y, highlights.y),
        color_balance_channel(rgb.b, params.first.z, params.second.z, highlights.z),
    );
    if params.points[0].w > 0.0 {
        let after = dot(balanced, vec3(0.299, 0.587, 0.114));
        if after > 0.0001 { balanced = clamp(balanced * (before / after), vec3(0.0), vec3(1.0)); }
    }
    return balanced;
}

/// Compositor's Add Noise hash and its uniform range, so the GPU draws the pattern the CPU reference
/// in `backdrop.rs` and `effects.rs` draws.
fn noise_hash(value: u32) -> u32 {
    var x = value;
    x ^= x >> 16u;
    x *= 0x7feb352du;
    x ^= x >> 15u;
    x *= 0x846ca68bu;
    x ^= x >> 16u;
    return x;
}

fn noise_unit(key: u32) -> f32 { return f32(noise_hash(key) >> 8u) * (1.0 / 16777216.0); }

fn noise_delta(key: u32, spread: f32, gaussian: bool) -> f32 {
    if gaussian {
        // Box–Muller: two uniform values make one normally distributed one.
        let u1 = noise_unit(key);
        let u2 = noise_unit(key ^ 0x68e31da4u);
        return sqrt(-2.0 * log(1.0 - u1)) * cos(6.2831853 * u2) * spread * (2.0 / 3.0);
    }
    return (noise_unit(key) * 2.0 - 1.0) * spread;
}

fn add_noise(rgb: vec3<f32>, point: vec2<f32>) -> vec3<f32> {
    let seed = bitcast<u32>(params.first.w);
    let x = u32(max(point.x, 0.0));
    let y = u32(max(point.y, 0.0));
    let base = noise_hash(seed ^ noise_hash(x * 0x9e3779b9u + y * 0x85ebca6bu));
    let spread = params.first.x / 100.0 * 127.5 / 255.0;
    let gaussian = params.first.y > 0.0;
    // One seed for all three channels when the noise is monochromatic, one each otherwise.
    let channel = select(0x9e3779b9u, 0u, params.first.z > 0.0);
    return rgb + vec3(
        noise_delta(base, spread, gaussian),
        noise_delta(base + channel, spread, gaussian),
        noise_delta(base + channel * 2u, spread, gaussian),
    );
}

fn adjust(rgb: vec3<f32>, point: vec2<f32>) -> vec3<f32> {
    let a = params.first;
    let b = params.second;
    switch params.flags.y {
        case 1u: { return adjust_hsv(rgb, a); }
        case 2u: { let value = pow(clamp((rgb * 255.0 - a.x) / max(a.z - a.x, 1.0), vec3(0.0), vec3(1.0)), vec3(1.0 / max(a.y, 0.01))); return (a.w + value * (b.x - a.w)) / 255.0; }
        case 3u: { return vec3(curve(rgb.r, 0u, params.flags.w), curve(rgb.g, 0u, params.flags.w), curve(rgb.b, 0u, params.flags.w)); }
        case 4u: {
            let linear = select(pow((rgb + 0.055) / 1.055, vec3(2.4)), rgb / 12.92, rgb <= vec3(0.04045));
            let result = pow(max(linear * exp2(a.x) + a.y, vec3(0.0)), vec3(1.0 / max(a.z, 0.01)));
            return select(1.055 * pow(result, vec3(1.0 / 2.4)) - 0.055, result * 12.92, result <= vec3(0.0031308));
        }
        case 5u: { return mix(a.rgb, b.rgb, dot(rgb, vec3(0.2126, 0.7152, 0.0722))); }
        case 6u: {
            let seed = bitcast<u32>(a.z);
            let p = vec2<u32>(point);
            let step = select(12345u, 0u, a.y > 0.0);
            return rgb + vec3(noise(p, seed), noise(p, seed + step), noise(p, seed + step * 2u)) * a.x / 100.0;
        }
        case 7u: { return 1.0 - rgb; }
        case 8u: { return vec3(channel_level(channel_level(rgb.r, 1u), 0u), channel_level(channel_level(rgb.g, 2u), 0u), channel_level(channel_level(rgb.b, 3u), 0u)); }
        case 9u: { return vec3(curve(curve(rgb.r, 32u, u32(a.y)), 0u, u32(a.x)), curve(curve(rgb.g, 64u, u32(a.z)), 0u, u32(a.x)), curve(curve(rgb.b, 96u, u32(a.w)), 0u, u32(a.x))); }
        case 10u: {
            if a.y > 0.0 { let selected = params.points[u32(a.x)]; return adjust_hsv(rgb, vec4(selected.xyz, 1.0)); }
            let high = max(max(rgb.r, rgb.g), rgb.b);
            let low = min(min(rgb.r, rgb.g), rgb.b);
            let delta = high - low;
            var hue = 0.0;
            if delta > 0.000001 {
                if high == rgb.r { hue = (rgb.g - rgb.b) / delta; }
                else if high == rgb.g { hue = (rgb.b - rgb.r) / delta + 2.0; }
                else { hue = (rgb.r - rgb.g) / delta + 4.0; }
                hue = wrap(hue * 60.0);
            }
            var response = params.points[0].xyz;
            for (var i = 1u; i < 7u; i++) {
                let band = params.points[i + 7u];
                let span = wrap(band.w - band.x);
                let position = wrap(hue - band.x);
                let ramp_in = wrap(band.y - band.x);
                let plateau_end = wrap(band.z - band.x);
                var weight = 1.0;
                if span > 0.0 {
                    if position > span { weight = 0.0; }
                    else if position < ramp_in { weight = position / max(ramp_in, 0.0001); }
                    else if position > plateau_end { weight = (span - position) / max(span - plateau_end, 0.0001); }
                }
                if a.z > 0.0 && u32(a.x) == i { weight = 1.0 - weight; }
                response += params.points[i].xyz * weight;
            }
            return adjust_hsv(rgb, vec4(response, 0.0));
        }
        case 11u: {
            let level = dot(rgb, vec3(0.2126, 0.7152, 0.0722));
            let delta = film_grain(point, a) * a.x / 100.0 * 0.35 * (0.4 + 2.4 * level * (1.0 - level));
            return rgb + delta;
        }
        case 12u: { return adjust_black_white(rgb); }
        case 13u: { return adjust_color_balance(rgb); }
        case 14u: { return add_noise(rgb, point); }
        default: { return rgb; }
    }
}

