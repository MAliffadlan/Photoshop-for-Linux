// Common raster buffer helpers precede this shader. Float intermediates stay on
// the device from camera conversion through detail enhancement and final encoding.
fn raw_luma(rgb: vec3<f32>) -> f32 { return dot(rgb, vec3(0.2126, 0.7152, 0.0722)); }

fn smooth_weight(value: f32) -> f32 {
    let t = clamp(value, 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn source_point(uv: vec2<f32>) -> vec2<f32> {
    let aspect = config[0].x / config[0].y;
    let p = (uv - 0.5) * 2.0 / vec2(1.0, aspect);
    var point =
        vec2(config[1].x * p.x + config[1].y * p.y, -config[1].y * p.x + config[1].x * p.y) *
        vec2(1.0, aspect);
    point /= max(1.0 + 0.004 * dot(config[1].zw, point), 0.2);
    return 0.5 + point * (1.0 + config[2].x * 0.003 * dot(point, point) * 0.5) * 0.5;
}

fn camera_at(p: vec2<i32>) -> vec3<f32> {
    let size = vec2<u32>(config[0].xy);
    let q = vec2<u32>(clamp(p, vec2(0), vec2<i32>(size) - 1));
    let i = (q.y * size.x + q.x) * 3u;
    return bitcast<vec3<f32>>(vec3(input[i], input[i + 1u], input[i + 2u]));
}

fn camera_sample(uv: vec2<f32>) -> vec3<f32> {
    let p = clamp(uv * config[0].xy - 0.5, vec2(0.0), config[0].xy - 1.0);
    let low = vec2<i32>(floor(p));
    let f = fract(p);
    return mix(mix(camera_at(low), camera_at(low + vec2(1, 0)), f.x),
               mix(camera_at(low + vec2(0, 1)), camera_at(low + vec2(1, 1)), f.x), f.y);
}

@compute @workgroup_size(8, 8)
fn raw_camera(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / config[0].xy;
    let source = source_point(uv);
    var camera = camera_sample(source);
    camera.r = camera_sample((source - 0.5) * (1.0 + config[2].y * 0.0002) + 0.5).r;
    camera.b = camera_sample((source - 0.5) * (1.0 + config[2].z * 0.0002) + 0.5).b;
    camera *= config[3].xyz * config[3].w;
    var rgb =
        vec3(dot(config[4].xyz, camera), dot(config[5].xyz, camera), dot(config[6].xyz, camera));
    let radial = dot(uv - 0.5, uv - 0.5) * 2.0;
    rgb = max(rgb * exp2(config[2].w * 0.03 * radial * radial), vec3(0.0));
    let alpha = select(0.0, 1.0, all(source >= vec2(0.0)) && all(source <= vec2(1.0)));
    store_float(id.y * size.x + id.x, vec4(rgb, alpha));
}

@compute @workgroup_size(8, 8)
fn raw_denoise(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let i = id.y * size.x + id.x;
    let center = load_float(i);
    let lum = raw_luma(center.rgb);
    var sum = vec3(0.0);
    var weights = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let p = vec2<u32>(clamp(vec2<i32>(id.xy) + vec2(x, y), vec2(0), vec2<i32>(size) - 1));
            let rgb = load_float(p.y * size.x + p.x).rgb;
            let delta = (raw_luma(rgb) - lum) / (0.015 + lum * 0.12);
            let weight = exp(-delta * delta);
            sum += rgb * weight;
            weights += weight;
        }
    }
    let mean = sum / max(weights, 0.00001);
    let mean_lum = raw_luma(mean);
    let destination = lum + (mean_lum - lum) * config[7].x / 100.0;
    let chroma =
        (center.rgb - lum) * (1.0 - config[7].y / 100.0) + (mean - mean_lum) * config[7].y / 100.0;
    store_float(i, vec4(max(destination + chroma, vec3(0.0)), center.a));
}

@compute @workgroup_size(8, 8)
fn raw_overlay(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / config[0].xy;
    let aspect = config[0].x / config[0].y;
    let delta = (config[1].zw - config[1].xy) * vec2(aspect, 1.0);
    let relative = (uv - config[1].xy) * vec2(aspect, 1.0);
    var weight = 0.0;
    if (config[2].x == 0.0) {
        weight = 1.0 - smooth_weight(dot(relative, delta) / max(dot(delta, delta), 0.00001));
    } else if (config[2].x == 1.0) {
        let distance = length(relative / max(abs(delta), vec2(0.001)));
        weight = 1.0 - smooth_weight((distance - (1.0 - config[2].y)) / config[2].y);
    } else {
        let tile = (id.y / 32u) * u32(config[4].x) + id.x / 32u;
        let header = config[5u + tile];
        for (var j = 0u; j < u32(header.y); j++) {
            let point = config[u32(header.x) + j].xy;
            let distance = length((uv - point) * config[0].xy) / (config[2].w * config[0].y);
            weight =
                max(weight, 1.0 - smooth_weight((distance - (1.0 - config[2].y)) / config[2].y));
        }
    }
    if (config[2].z != 0.0) {
        weight = 1.0 - weight;
    }
    let i = id.y * size.x + id.x;
    let p = load_float(i);
    let gain = exp2(config[3].x * weight);
    let warmth = exp2(config[3].y * weight / 200.0);
    let rgb = p.rgb * gain * vec3(warmth, 1.0, 1.0 / warmth);
    let lum = raw_luma(rgb);
    store_float(
        i, vec4(max(lum + (rgb - lum) * (1.0 + config[3].z * weight / 100.0), vec3(0.0)), p.a));
}

fn raw_curve(value: f32, channel: u32) -> f32 {
    let x = clamp(value, 0.0, 1.0) * 4.0;
    let i = min(u32(x), 3u);
    let base = 16u + channel * 2u;
    let a = config[base][i];
    var b = config[base + 1u].x;
    if (i < 3u) {
        b = config[base][i + 1u];
    }
    return a + (b - a) * (x - f32(i));
}

fn raw_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let high = max(max(rgb.r, rgb.g), rgb.b);
    let low = min(min(rgb.r, rgb.g), rgb.b);
    let delta = high - low;
    let light = (high + low) * 0.5;
    if (delta < 0.00001) {
        return vec3(0.0, 0.0, light);
    }
    var hue = (rgb.r - rgb.g) / delta + 4.0;
    if (high == rgb.r) {
        hue = (rgb.g - rgb.b) / delta;
    } else if (high == rgb.g) {
        hue = (rgb.b - rgb.r) / delta + 2.0;
    }
    return vec3(((hue * 60.0) % 360.0 + 360.0) % 360.0,
                delta / max(1.0 - abs(2.0 * light - 1.0), 0.00001), light);
}

fn raw_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let c = (1.0 - abs(2.0 * hsl.z - 1.0)) * hsl.y;
    let h = ((hsl.x % 360.0) + 360.0) % 360.0 / 60.0;
    let x = c * (1.0 - abs(h % 2.0 - 1.0));
    var rgb = vec3(c, 0.0, x);
    if (h < 1.0) {
        rgb = vec3(c, x, 0.0);
    } else if (h < 2.0) {
        rgb = vec3(x, c, 0.0);
    } else if (h < 3.0) {
        rgb = vec3(0.0, c, x);
    } else if (h < 4.0) {
        rgb = vec3(0.0, x, c);
    } else if (h < 5.0) {
        rgb = vec3(x, 0.0, c);
    }
    return rgb + hsl.z - c * 0.5;
}

// Compositor's calibration: a hue rotation for everything darker than a fixed
// lightness, and a hue and saturation shift for whichever primary dominates the
// pixel. config[41] carries the shadow tint, the red primary and the process
// version; config[42] the other two primaries. The process scales all of it, and
// the lightness is never touched.
fn raw_calibrate(rgb: vec3<f32>) -> vec3<f32> {
    let calibration = config[41];
    let others = config[42];
    if (calibration.x == 0.0 && calibration.y == 0.0 && calibration.z == 0.0 && others.x == 0.0 &&
        others.y == 0.0 && others.z == 0.0 && others.w == 0.0) {
        return rgb;
    }
    let scale = calibration_scale(calibration.w);
    var hsl = raw_hsl(rgb);
    if (hsl.z < 0.35) {
        // Compositor counts hue in turns; 0.06 of a turn is the 21.6 degrees
        // this panel's slider reaches.
        hsl.x += calibration.x * 21.6 * scale;
    }
    // One primary at a time, and only where there is one to speak of.
    let highest = max(rgb.r, max(rgb.g, rgb.b));
    let lowest = min(rgb.r, min(rgb.g, rgb.b));
    if (highest - lowest > 1e-5) {
        var hue = 0.0;
        var saturation = 0.0;
        if (rgb.r >= rgb.g && rgb.r >= rgb.b) {
            hue = calibration.y;
            saturation = calibration.z;
        } else if (rgb.g >= rgb.r && rgb.g >= rgb.b) {
            hue = others.x;
            saturation = others.y;
        } else {
            hue = others.z;
            saturation = others.w;
        }
        hsl.x += hue * 15.0 * scale;
        hsl.y = clamp(hsl.y * (1.0 + saturation * 0.45 * scale), 0.0, 1.0);
    }
    hsl.x = hsl.x - floor(hsl.x / 360.0) * 360.0;
    return clamp(raw_rgb(hsl), vec3(0.0), vec3(1.0));
}

// How hard a process version pushes the calibration it is given.
fn calibration_scale(version: f32) -> f32 {
    if (version <= 1.0) { return 0.55; }
    if (version <= 2.0) { return 0.65; }
    if (version <= 3.0) { return 0.75; }
    if (version <= 4.0) { return 0.85; }
    if (version <= 5.0) { return 0.92; }
    return 1.0;
}

// The colour grading wheels, in Compositor's order: the three tonal wheels
// weighted by how dark or bright the pixel is, then the global one at full
// weight. config[15] carries the blending, the balance and whether any wheel
// asks for a change at all, so a neutral grading costs nothing and leaves the
// pixels exactly as the tone work left them.
fn raw_grade(rgb: vec3<f32>) -> vec3<f32> {
    if (config[15].z == 0.0) {
        return rgb;
    }
    var color = rgb;
    let tone = raw_luma(color);
    let split = 0.5 - config[15].y / 100.0 * 0.2;
    let reach = 0.12 + config[15].x / 100.0 * 0.38;
    let span = max(reach * 2.0, 0.05);
    var weights = vec4(clamp((split + reach - tone) / span, 0.0, 1.0),
                       clamp(1.0 - abs(tone - split) / (0.35 + reach), 0.0, 1.0),
                       clamp((tone - (split - reach)) / span, 0.0, 1.0), 1.0);
    let sum = weights.x + weights.y + weights.z;
    if (sum > 0.0001) {
        weights = vec4(weights.xyz / sum, 1.0);
    }
    for (var wheel_index = 0u; wheel_index < 4u; wheel_index++) {
        let wheel = config[32u + wheel_index];
        let weight = weights[wheel_index];
        if (wheel.w == 0.0 || weight <= 0.0) {
            continue;
        }
        if (wheel.y > 0.0) {
            let tint = raw_rgb(vec3(wheel.x, 1.0, 0.5));
            color += (tint - 0.5) * (wheel.y / 100.0 * weight * 0.85);
        }
        if (wheel.z != 0.0) {
            let luma = max(raw_luma(color), 0.00001);
            let wanted = clamp(luma + wheel.z / 100.0 * 0.25 * weight, 0.0, 1.0);
            color = clamp(color * (wanted / luma), vec3(0.0), vec3(1.0));
        }
    }
    return clamp(color, vec3(0.0), vec3(1.0));
}

@compute @workgroup_size(8, 8)
fn raw_tone(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let i = id.y * size.x + id.x;
    let p = load_float(i);
    var rgb = p.rgb;
    let lum = max(raw_luma(rgb), 0.00001);
    let shadow = pow(1.0 - clamp(lum / 0.5, 0.0, 1.0), 2.0);
    let highlight = smooth_weight((lum - 0.18) / 0.82);
    let gain = exp2((config[8].x * shadow + config[8].y * highlight) / 50.0);
    rgb = max(rgb * gain * exp2(config[8].z / 100.0) + config[8].w / 1000.0, vec3(0.0));
    rgb = max((rgb - config[9].x / 1000.0) / (1.0 - config[9].x / 500.0), vec3(0.0));
    rgb = pow(rgb, vec3(exp2(-config[9].y / 100.0)));
    rgb = select(1.055 * pow(rgb, vec3(1.0 / 2.4)) - 0.055, rgb * 12.92, rgb <= vec3(0.0031308));
    rgb = raw_calibrate(rgb);
    rgb = clamp((rgb - 0.5) * exp2(config[9].z / 100.0) + 0.5, vec3(0.0), vec3(1.0));
    let excess = max((rgb.r + rgb.b) * 0.5 - rgb.g, 0.0) * config[9].w / 100.0;
    rgb -= vec3(excess, 0.0, excess);
    rgb = vec3(raw_curve(raw_curve(rgb.r, 0u), 1u), raw_curve(raw_curve(rgb.g, 0u), 2u),
               raw_curve(raw_curve(rgb.b, 0u), 3u));
    var hsl = raw_hsl(rgb);
    var change = vec3(0.0);
    for (var band = 0u; band < 8u; band++) {
        let row = config[24u + band];
        let distance = abs(((hsl.x - row.w + 180.0) % 360.0 + 360.0) % 360.0 - 180.0);
        change += row.xyz * max(1.0 - distance / 45.0, 0.0);
    }
    hsl.x += change.x * 0.3;
    hsl.y = clamp(hsl.y * (1.0 + config[10].x / 100.0 + change.y / 100.0) *
                      (1.0 + config[10].y / 100.0 * (1.0 - hsl.y)),
                  0.0, 1.0);
    hsl.z = clamp(hsl.z + change.z / 200.0, 0.0, 1.0);
    rgb = raw_rgb(hsl);
    if (config[10].z != 0.0) {
        rgb = vec3(clamp(dot(rgb, config[11].xyz), 0.0, 1.0));
    }
    let high = smooth_weight(raw_luma(rgb) + config[10].w / 200.0);
    let tones = config[12];
    rgb = mix(rgb, raw_rgb(vec3(tones.x, 1.0, 0.5)), tones.y / 100.0 * (1.0 - high) * 0.35);
    rgb = mix(rgb, raw_rgb(vec3(tones.z, 1.0, 0.5)), tones.w / 100.0 * high * 0.35);
    rgb = raw_grade(rgb);
    store_float(i, vec4(rgb, p.a));
}

// Compositor's glow source: how much of the picture is bright enough to glow.
// It reads the luma of the encoded value, which is where the blur starts, and
// Compositor takes that reading before texture and clarity have their say.
@compute @workgroup_size(8, 8)
fn raw_glow_source(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let i = id.y * size.x + id.x;
    let p = load_float(i);
    let threshold = 0.55 + 0.4 * (config[36].z / 100.0);
    let span = max(1.0 - threshold, 0.05);
    store_float(i, vec4(clamp((raw_luma(p.rgb) - threshold) / span, 0.0, 1.0), 0.0, 0.0, p.a));
}

// One separable box blur over the glow plane, edge-clamped, along the direction
// config[1].zw points. Compositor's kernel is a box and not a Gaussian: the
// shape of the falloff is the look, so this walks the window rather than
// sharing the Gaussian the sharpening passes use.
@compute @workgroup_size(8, 8)
fn raw_glow_blur(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    // config[1] carries the radius this pass runs at and the direction it runs
    // in, so the blur does not carry the whole develop's settings.
    let radius = i32(max(config[1].x, 1.0));
    let step = vec2<i32>(i32(config[1].z), i32(config[1].w));
    let limit = vec2<i32>(size) - vec2<i32>(1);
    var sum = 0.0;
    for (var k = -radius; k <= radius; k++) {
        let at = clamp(vec2<i32>(id.xy) + step * k, vec2<i32>(0), limit);
        sum += bitcast<f32>(auxiliary[(u32(at.y) * size.x + u32(at.x)) * 4u]);
    }
    let i = id.y * size.x + id.x;
    let alpha = bitcast<f32>(auxiliary[i * 4u + 3u]);
    store_float(i, vec4(sum / f32(radius * 2 + 1), 0.0, 0.0, alpha));
}

// Compositor's post-grade effects, in its order: the glow lands on the picture,
// the vignette reads the result, and grain goes on top of both. Compositor
// stores after every one of them, so this clamps in the same places.
@compute @workgroup_size(8, 8)
fn raw_effects(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let i = id.y * size.x + id.x;
    let p = load_float(i);
    var rgb = p.rgb;
    let glow = config[36];
    if (glow.x > 0.0) {
        // Halation's fringe is red and warmth only pushes it further that way;
        // the other two start cool and warm across all three channels, and
        // Bloom lands harder.
        let warmth = config[37].x / 100.0;
        var tint = vec3(0.75 + 0.25 * warmth, 0.6 + 0.2 * warmth, 0.75 - 0.6 * warmth);
        var gain = 1.0;
        if (glow.y == 1.0) {
            gain = 1.4;
        }
        if (glow.y == 2.0) {
            tint = vec3(1.0, 0.35 - 0.3 * warmth, 0.2 - 0.2 * warmth);
        }
        let plane = bitcast<f32>(auxiliary[i * 4u]) * (glow.x / 100.0) * gain;
        rgb = clamp(rgb + plane * tint, vec3(0.0), vec3(1.0));
    }
    let vignette = config[38];
    if (vignette.x != 0.0) {
        // Roundness is Compositor's: positive rounds the falloff toward a circle
        // and negative squares it. The midpoint is measured against the frame's
        // half-diagonal, so the corners always fall away hardest.
        let nx = (f32(id.x) + 0.5) / f32(size.x) * 2.0 - 1.0;
        let ny = (f32(id.y) + 0.5) / f32(size.y) * 2.0 - 1.0;
        let square = max(abs(nx), abs(ny));
        let circle = length(vec2<f32>(nx, ny)) / sqrt(2.0);
        let distance = circle + (square - circle) * vignette.w;
        let mask = smooth_weight((distance - vignette.z) / config[39].x);
        var effect = vignette.x / 100.0 * mask;
        // Only a darkening vignette has bright pixels to protect, and only
        // Highlight Priority protects them.
        if (effect < 0.0 && vignette.y == 0.0) {
            let bright = clamp((raw_luma(rgb) - 0.45) / 0.55, 0.0, 1.0);
            effect *= 1.0 - config[39].y / 100.0 * bright;
        }
        if (effect < 0.0) {
            rgb *= 1.0 + effect;
        } else if (effect > 0.0) {
            rgb += (1.0 - rgb) * effect;
        }
        rgb = clamp(rgb, vec3(0.0), vec3(1.0));
        // Color Priority also pulls the colour out of the edges it darkens.
        // Paint Overlay is the plain vignette here, like Compositor's kernel.
        if (vignette.y == 1.0 && mask > 0.0) {
            let lum = raw_luma(rgb);
            let saturation = 1.0 - 0.75 * mask * abs(vignette.x / 100.0);
            rgb = clamp(vec3<f32>(lum) + (rgb - vec3<f32>(lum)) * saturation, vec3(0.0), vec3(1.0));
        }
    }
    let grain = config[40];
    if (grain.x > 0.0) {
        // Compositor measures grain in document pixels while this pass walks
        // preview pixels, so the pattern stays put in the picture.
        let point = (vec2<f32>(id.xy) + 0.5) * grain.w;
        // Roughness fades in the finer scale, whose particles keep their size
        // proportional to Size rather than collapsing onto single pixels.
        let coarse = raw_grain_field(point, grain.y, 0u);
        let fine = raw_grain_field(point, max(grain.y * 0.35, 0.5), raw_mix32(0xa511e9b3u));
        let noise = mix(coarse, fine, grain.z);
        let level = clamp(raw_luma(rgb), 0.0, 1.0);
        // Film grain shows most in the midtones, and least in the extremes.
        let delta = noise * (grain.x / 100.0 * 0.35) * (0.4 + 2.4 * level * (1.0 - level));
        rgb = clamp(rgb + vec3<f32>(delta), vec3(0.0), vec3(1.0));
    }
    store_float(i, vec4(rgb, p.a));
}

// Compositor's grain field: one scale, plus the finer one Roughness fades in.
// The same three functions serve adjustments.wgsl's film grain, kept in step
// with the CPU kernel in src/effects.rs.
fn raw_mix32(value: u32) -> u32 {
    var mixed = (value ^ (value >> 16u)) * 0x7feb352du;
    mixed = (mixed ^ (mixed >> 15u)) * 0x846ca68bu;
    return mixed ^ (mixed >> 16u);
}

fn raw_lattice(point: vec2<i32>, seed: u32) -> f32 {
    let hash = raw_mix32(bitcast<u32>(point.x) * 0x9e3779b1u ^
                         raw_mix32(bitcast<u32>(point.y) * 0x85ebca77u ^ seed));
    return f32(hash & 65535u) / 65535.0 + f32(hash >> 16u) / 65535.0 - 1.0;
}

fn raw_grain_field(point: vec2<f32>, size: f32, seed: u32) -> f32 {
    let cell = point / max(size, 0.0001);
    let origin = vec2<i32>(floor(cell));
    let f = fract(cell);
    let t = f * f * (3.0 - 2.0 * f);
    let top = mix(raw_lattice(origin, seed), raw_lattice(origin + vec2(1, 0), seed), t.x);
    let bottom =
        mix(raw_lattice(origin + vec2(0, 1), seed), raw_lattice(origin + vec2(1, 1), seed), t.x);
    return mix(top, bottom, t.y) * 1.6;
}

@compute @workgroup_size(8, 8)
fn raw_detail(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let i = id.y * size.x + id.x;
    let p = load_float(i);
    let b =
        bitcast<vec3<f32>>(vec3(auxiliary[i * 4u], auxiliary[i * 4u + 1u], auxiliary[i * 4u + 2u]));
    let lum = raw_luma(p.rgb);
    let delta = lum - raw_luma(b);
    var gain = 1.0;
    if (abs(delta) >= config[1].y) {
        gain = max(lum + delta * config[1].x, 0.0) / max(lum, 0.00001);
    }
    store_float(i, vec4(p.rgb * gain, p.a));
}

@compute @workgroup_size(8, 8)
fn raw_encode(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[13].zw);
    if (any(id.xy >= size)) {
        return;
    }
    let source = id.xy + vec2<u32>(config[13].xy);
    let p = load_float(source.y * u32(config[0].x) + source.x);
    let i = id.y * size.x + id.x;
    if (config[14].x == 8.0) {
        result[i] = packed(p);
    } else {
        let v = vec4<u32>(floor(clamp(p, vec4(0.0), vec4(1.0)) * 65535.0 + 0.5));
        result[i * 2u] = v.r | (v.g << 16u);
        result[i * 2u + 1u] = v.b | (v.a << 16u);
    }
}
