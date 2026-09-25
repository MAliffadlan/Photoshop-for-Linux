// Format: 0 straight RGBA8 -> premultiplied float; 1 mask; 2 camera RGB32F.
// config[1].y selects the historical integer premultiplication of Gaussian Blur.
@compute @workgroup_size(8, 8)
fn decode_pixels(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let i = id.y * size.x + id.x;
    var p = vec4(0.0);
    if (config[1].x == 0.0) {
        p = rgba(i);
        p = vec4(p.rgb * p.a, p.a);
        if (config[1].y != 0.0) {
            p = floor(p * 255.0 + 0.00001) / 255.0;
        }
    } else if (config[1].x == 1.0) {
        p = vec4(byte_at(i));
    } else {
        p = vec4(bitcast<f32>(input[i * 3u]), bitcast<f32>(input[i * 3u + 1u]),
                 bitcast<f32>(input[i * 3u + 2u]), 1.0);
    }
    store_float(i, p);
}

@compute @workgroup_size(8, 8)
fn encode_pixels(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let i = id.y * size.x + id.x;
    var p = load_float(i);
    if (config[1].x == 0.0) {
        if (config[1].y != 0.0) {
            p = floor(clamp(p, vec4(0.0), vec4(1.0)) * 255.0 + 0.5);
            p = vec4(select(p.rgb, floor(p.rgb * 255.0 / max(p.a, 1.0)), p.a > 0.0), p.a) / 255.0;
        } else {
            let alpha = clamp(p.a, 0.0, 1.0);
            p = vec4(p.rgb / max(alpha, 0.00001), alpha);
        }
        result[i] = packed(p);
    } else if (config[1].x == 1.0) {
        result[i] = u32(floor(clamp(p.x, 0.0, 1.0) * 255.0 + 0.5));
    } else {
        result[i * 3u] = bitcast<u32>(p.r);
        result[i * 3u + 1u] = bitcast<u32>(p.g);
        result[i * 3u + 2u] = bitcast<u32>(p.b);
    }
}

@compute @workgroup_size(8, 8)
fn gaussian(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let radius = i32(config[1].x);
    let direction = vec2<i32>(config[1].yz);
    var sum = vec4(0.0);
    for (var k = -radius; k <= radius; k++) {
        let p = vec2<u32>(clamp(vec2<i32>(id.xy) + direction * k, vec2(0), vec2<i32>(size) - 1));
        sum += load_float(p.y * size.x + p.x) * config[u32(k + radius) + 2u].x;
    }
    store_float(id.y * size.x + id.x, sum);
}

@compute @workgroup_size(8, 8)
fn resample(@builtin(global_invocation_id) id: vec3<u32>) {
    let source = vec2<u32>(config[0].xy);
    let destination = vec2<u32>(config[0].zw);
    if (any(id.xy >= destination)) {
        return;
    }
    let horizontal = config[1].x != 0.0;
    let axis = select(id.y, id.x, horizontal);
    let header = config[axis + 2u];
    var sum = vec4(0.0);
    for (var k = 0u; k < u32(header.y); k++) {
        let p = select(vec2(id.x, u32(header.x) + k), vec2(u32(header.x) + k, id.y), horizontal);
        sum += load_float(p.y * source.x + p.x) * config[u32(header.z) + k].x;
    }
    // image::resize clamps only the final horizontal pass, including float images.
    if (horizontal) {
        sum = clamp(sum, vec4(0.0), vec4(1.0));
    }
    store_float(id.y * destination.x + id.x, sum);
}

fn sampled(uv: vec2<f32>, size: vec2<u32>) -> vec4<f32> {
    if (any(uv < vec2(0.0)) || any(uv >= vec2(1.0))) {
        return vec4(0.0);
    }
    let pos = uv * vec2<f32>(size) - 0.5;
    let low = vec2<i32>(floor(pos));
    let f = fract(pos);
    var sum = vec4(0.0);
    for (var k = 0u; k < 4u; k++) {
        let p = vec2<u32>(
            clamp(low + vec2<i32>(i32(k % 2u), i32(k / 2u)), vec2(0), vec2<i32>(size) - 1));
        let c = rgba(p.y * size.x + p.x);
        let weight = select(1.0 - f.x, f.x, k % 2u == 1u) * select(1.0 - f.y, f.y, k / 2u == 1u);
        sum += vec4(c.rgb * c.a, c.a) * weight;
    }
    return vec4(sum.rgb / max(sum.a, 0.000001), sum.a);
}

@compute @workgroup_size(8, 8)
fn lens(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(size) * 2.0 - 1.0;
    let radius = dot(uv, uv);
    let k = 1.0 + config[1].x * radius / 100.0;
    let p = sampled((uv * k + 1.0) * 0.5, size);
    result[id.y * size.x + id.x] = packed(vec4(p.rgb * (1.0 - config[1].y * radius * 0.005), p.a));
}

fn auxiliary_rgba(index: u32) -> vec4<f32> {
    return unpack4x8unorm(auxiliary[index]);
}

fn rec709(rgb: vec3<f32>) -> f32 {
    return dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn vignette_mask_at(point: vec2<f32>, size: vec2<f32>, origin: vec2<f32>,
                    midpoint: f32, roundness: f32, feather: f32) -> f32 {
    let local = point - origin;
    let uv = local / max(size, vec2<f32>(1.0)) * 2.0 - vec2<f32>(1.0);
    let square = max(abs(uv.x), abs(uv.y));
    let circle = length(uv) / sqrt(2.0);
    let shape = (1.0 - roundness / 100.0) * 0.5;
    let distance = circle + (square - circle) * shape;
    let start = midpoint / 100.0 * 0.85;
    let softness = max(feather / 100.0, 0.05);
    let t = clamp((distance - start) / softness, 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

@compute @workgroup_size(8, 8)
fn vignette(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let index = id.y * size.x + id.x;
    let source = rgba(index);
    let fill_empty = config[3].z > 0.5;
    if (source.a <= 0.0 && !fill_empty) {
        result[index] = packed(source);
        return;
    }
    let mask = vignette_mask_at(
        vec2<f32>(id.xy) + vec2<f32>(0.5),
        config[3].xy,
        config[0].zw,
        config[1].y,
        config[1].z,
        config[1].w,
    );
    if (mask <= 0.0) {
        result[index] = packed(source);
        return;
    }
    let rgb = select(
        vec3<f32>(0.0),
        min(source.rgb, vec3<f32>(1.0)),
        source.a > 0.0,
    );
    let bright = clamp((rec709(rgb) - 0.45) / 0.55, 0.0, 1.0);
    let effect = clamp(config[1].x / 100.0, 0.0, 1.0) * mask
        * (1.0 - config[2].x / 100.0 * bright);
    let color = vec3<f32>(config[2].y, config[2].z, config[2].w);
    var out_alpha = source.a;
    var out_rgb = rgb;
    if (fill_empty) {
        out_alpha = clamp(source.a + effect * (1.0 - source.a), 0.0, 1.0);
        if (out_alpha <= 0.0) {
            out_rgb = vec3<f32>(0.0);
        } else {
            out_rgb = (color * effect + rgb * source.a * (1.0 - effect)) / out_alpha;
        }
    } else {
        out_rgb = rgb + (color - rgb) * effect;
    }
    result[index] = packed(vec4(out_rgb, out_alpha));
}

@compute @workgroup_size(8, 8)
fn bloom_source(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let index = id.y * size.x + id.x;
    let source = rgba(index);
    if (source.a <= 0.0) {
        result[index] = 0u;
        return;
    }
    let rgb = min(source.rgb, vec3<f32>(1.0));
    let luminance = rec709(rgb);
    let highlight = clamp((luminance - config[1].x) / (1.0 - config[1].x), 0.0, 1.0);
    result[index] = packed(vec4(rgb * highlight, source.a * highlight));
}

@compute @workgroup_size(8, 8)
fn bloom_final(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let index = id.y * size.x + id.x;
    let source = rgba(index);
    let glow_image = auxiliary_rgba(index);
    let glow_alpha = glow_image.a;
    let glow_rgb = min(glow_image.rgb, vec3<f32>(1.0));
    let highlight = glow_alpha * config[1].x;
    let out_alpha = clamp(source.a + highlight, 0.0, 1.0);
    if (out_alpha <= 0.0) {
        result[index] = packed(source);
        return;
    }
    let out_rgb = (source.rgb * source.a + glow_rgb * highlight) / out_alpha;
    result[index] = packed(vec4(out_rgb, out_alpha));
}

fn tonal_smooth(low: f32, high: f32, value: f32) -> f32 {
    let t = clamp((value - low) / (high - low), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

@compute @workgroup_size(8, 8)
fn tonal_contrast(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = vec2<u32>(config[0].xy);
    if (any(id.xy >= size)) {
        return;
    }
    let index = id.y * size.x + id.x;
    let source = rgba(index);
    let base = auxiliary_rgba(index);
    if (source.a <= 0.0 || base.a <= 0.0) {
        result[index] = packed(source);
        return;
    }
    let rgb = min(source.rgb, vec3<f32>(1.0));
    let base_rgb = min(base.rgb, vec3<f32>(1.0));
    let luminance = rec709(rgb);
    let base_luminance = rec709(base_rgb);
    let shadow_weight = 1.0 - tonal_smooth(0.15, 0.5, base_luminance);
    let highlight_weight = tonal_smooth(0.5, 0.85, base_luminance);
    let midtone_weight = 1.0 - shadow_weight - highlight_weight;
    let weight = (config[1].y * shadow_weight
        + config[1].z * midtone_weight
        + config[1].w * highlight_weight) / 100.0;
    let detail = luminance - base_luminance;
    let delta = 0.18 * tanh(detail * 6.0) * weight
        * (config[1].x / 50.0) * (4.0 * luminance * (1.0 - luminance));
    result[index] = packed(vec4(clamp(rgb + vec3<f32>(delta), vec3<f32>(0.0), vec3<f32>(1.0)), source.a));
}
