@group(0) @binding(0) var previous: texture_2d<f32>;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var coverage: texture_2d<f32>;
@group(0) @binding(3) var output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var<uniform> params: Parameters;
@group(0) @binding(5) var display: texture_storage_2d<rgba8unorm, write>;

fn source_pixel(pixel: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(source));
    let p = textureLoad(source, clamp(pixel, vec2(0), size - 1), 0);
    return vec4(p.rgb * p.a, p.a);
}

fn sample_source(uv: vec2<f32>) -> vec4<f32> {
    if any(uv < vec2(0.0)) || any(uv >= vec2(1.0)) { return vec4(0.0); }
    let point = uv * vec2<f32>(textureDimensions(source)) - 0.5;
    let low = vec2<i32>(floor(point));
    let fraction = fract(point);
    let a = mix(source_pixel(low), source_pixel(low + vec2(1, 0)), fraction.x);
    let b = mix(source_pixel(low + vec2(0, 1)), source_pixel(low + vec2(1, 1)), fraction.x);
    let p = mix(a, b, fraction.y);
    return vec4(select(vec3(0.0), p.rgb / max(p.a, 0.000001), p.a > 0.0), p.a);
}

fn motion_pixel(pixel: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(source));
    // Filtering a padded layer uses transparent texels outside the original image.
    if any(pixel < vec2(0)) || any(pixel >= size) { return vec4(0.0); }
    let p = textureLoad(source, pixel, 0);
    return vec4(p.rgb * p.a, p.a);
}

fn sample_motion_blur(uv: vec2<f32>) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(source));
    let extent = abs(params.appearance.zw) * 0.5 + 0.5 / size;
    if any(uv < -extent) || any(uv > 1.0 + extent) { return vec4(0.0); }
    let steps = u32(params.appearance.y);
    var sum = vec4(0.0);
    for (var i = 0u; i < steps; i++) {
        let offset = (f32(i) + 0.5) / f32(steps) - 0.5;
        let point = (uv + offset * params.appearance.zw) * size - 0.5;
        let low = vec2<i32>(floor(point));
        let fraction = fract(point);
        let a = mix(motion_pixel(low), motion_pixel(low + vec2(1, 0)), fraction.x);
        let b = mix(motion_pixel(low + vec2(0, 1)), motion_pixel(low + vec2(1, 1)), fraction.x);
        sum += mix(a, b, fraction.y);
    }
    return vec4(sum.rgb / max(sum.a, 0.000001), sum.a / f32(steps));
}

// The Gaussian tap count and the σ = radius / 2 weights `backdrop.rs` uses on the CPU: the two
// renderers have to keep step, so the taps are computed here rather than passed in.
const GAUSSIAN_TAPS: u32 = 32u;

fn gaussian_weight(offset: f32, radius: f32) -> f32 {
    let sigma = max(radius * 0.5, 0.0001);
    return exp(-(offset * offset) / (2.0 * sigma * sigma));
}

// A Gaussian tap, spread evenly over ±radius like the reference's.
fn gaussian_offset(index: u32, radius: f32) -> f32 {
    return ((f32(index) + 0.5) / f32(GAUSSIAN_TAPS) * 2.0 - 1.0) * radius;
}

// The first axis reads the sharp accumulation, which is straight color with alpha, so it is
// premultiplied as it is read: a transparent edge must not pull the softening towards black. The
// second axis reads what the first one wrote, which is already premultiplied. `combine_backdrop`
// undoes the premultiplication once, at the end.
fn previous_pixel(pixel: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(previous));
    if any(pixel < vec2(0)) || any(pixel >= size) { return vec4(0.0); }
    let p = textureLoad(previous, pixel, 0);
    return vec4(p.rgb * p.a, p.a);
}

// The result of the horizontal pass: already premultiplied, so it is read as it stands.
fn scratch_pixel(pixel: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(previous));
    if any(pixel < vec2(0)) || any(pixel >= size) { return vec4(0.0); }
    return textureLoad(previous, pixel, 0);
}

// Texel coordinates, so an integer lands on a pixel centre exactly as the CPU samples it.
fn previous_sample(texel: vec2<f32>) -> vec4<f32> {
    let low = vec2<i32>(floor(texel));
    let fraction = fract(texel);
    let a = mix(previous_pixel(low), previous_pixel(low + vec2(1, 0)), fraction.x);
    let b = mix(previous_pixel(low + vec2(0, 1)), previous_pixel(low + vec2(1, 1)), fraction.x);
    return mix(a, b, fraction.y);
}

fn scratch_sample(texel: vec2<f32>) -> vec4<f32> {
    let low = vec2<i32>(floor(texel));
    let fraction = fract(texel);
    let a = mix(scratch_pixel(low), scratch_pixel(low + vec2(1, 0)), fraction.x);
    let b = mix(scratch_pixel(low + vec2(0, 1)), scratch_pixel(low + vec2(1, 1)), fraction.x);
    return mix(a, b, fraction.y);
}

// The backdrop, premultiplied so transparent surroundings never pull a blur towards black. Anything
// outside the canvas counts as transparent, as it does for the reference's out-of-range reads.
fn backdrop_pixel(pixel: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(source));
    if any(pixel < vec2(0)) || any(pixel >= size) { return vec4(0.0); }
    let p = textureLoad(source, pixel, 0);
    return vec4(p.rgb * p.a, p.a);
}

fn backdrop_sample(texel: vec2<f32>) -> vec4<f32> {
    let low = vec2<i32>(floor(texel));
    let fraction = fract(texel);
    let a = mix(backdrop_pixel(low), backdrop_pixel(low + vec2(1, 0)), fraction.x);
    let b = mix(backdrop_pixel(low + vec2(0, 1)), backdrop_pixel(low + vec2(1, 1)), fraction.x);
    return mix(a, b, fraction.y);
}

fn backdrop_amount(position: vec2<i32>) -> f32 {
    var amount = params.appearance.x;
    if params.flags.z != 0u { amount *= textureLoad(coverage, position, 0).r; }
    return amount;
}

// Lay the softened backdrop over the sharp one. Both sides are premultiplied while they are mixed, so
// the color of a transparent pixel cannot leak into its neighbours, and the layer's own opacity thins
// the softened result the way it thins anything else the layer draws.
fn combine_backdrop(sharp: vec4<f32>, blurred: vec4<f32>, amount: f32) -> vec4<f32> {
    let kept = clamp(amount, 0.0, 1.0);
    let mixed = sharp.rgb * sharp.a * (1.0 - kept) + blurred.rgb * kept;
    let alpha = sharp.a * (1.0 - kept) + blurred.a * kept;
    return vec4(select(vec3(0.0), mixed / max(alpha, 0.000001), alpha > 0.000001), alpha);
}

@compute @workgroup_size(8, 8)
fn composite(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= vec2<u32>(params.canvas.xy)) { return; }
    let position = vec2<i32>(id.xy);
    // The backdrop blur passes come first: the first of the pair reads a scratch texture rather than
    // the accumulation, which is also what the second one writes. Their modes sit above the adjustment
    // kinds, which already run from 1 to 14.
    if params.flags.y == 20u {
        let radius = params.first.x;
        var sum = vec4(0.0);
        var total = 0.0;
        for (var i = 0u; i < GAUSSIAN_TAPS; i++) {
            let offset = gaussian_offset(i, radius);
            let weight = gaussian_weight(offset, radius);
            sum += previous_sample(vec2<f32>(position) + vec2(offset, 0.0)) * weight;
            total += weight;
        }
        textureStore(output, position, sum / total);
        return;
    }
    if params.flags.y == 21u {
        let radius = params.first.x;
        var sum = vec4(0.0);
        var total = 0.0;
        for (var i = 0u; i < GAUSSIAN_TAPS; i++) {
            let offset = gaussian_offset(i, radius);
            let weight = gaussian_weight(offset, radius);
            sum += scratch_sample(vec2<f32>(position) + vec2(0.0, offset)) * weight;
            total += weight;
        }
        textureStore(output, position, combine_backdrop(
            textureLoad(source, position, 0), sum / total, backdrop_amount(position)));
        return;
    }
    if params.flags.y == 22u {
        let steps = max(u32(params.appearance.y), 1u);
        var sum = vec4(0.0);
        for (var i = 0u; i < steps; i++) {
            let offset = (f32(i) + 0.5) / f32(steps) - 0.5;
            sum += backdrop_sample(vec2<f32>(position) + offset * params.appearance.zw * params.canvas.xy);
        }
        textureStore(output, position, combine_backdrop(
            textureLoad(source, position, 0), sum / f32(steps), backdrop_amount(position)));
        return;
    }
    let dst = textureLoad(previous, position, 0);
    if params.flags.y >= 100u {
        textureStore(display, position, select(vec4(dst.rgb * dst.a, dst.a), dst, params.flags.y == 101u));
        return;
    }
    let point = (vec2<f32>(id.xy) + 0.5) / params.canvas.xy * params.canvas.zw;
    var amount = params.appearance.x;
    if params.flags.z != 0u { amount *= textureLoad(coverage, position, 0).r; }
    if params.flags.y != 0u {
        textureStore(output, position, vec4(mix(dst.rgb, clamp(adjust(dst.rgb, point), vec3(0.0), vec3(1.0)), amount), dst.a));
        return;
    }
    let local = point - params.bounds.xy - params.bounds.zw * 0.5;
    var uv = vec2(local.x * params.rotation.x + local.y * params.rotation.y,
        -local.x * params.rotation.y + local.y * params.rotation.x) / params.bounds.zw;
    uv = uv * params.rotation.zw + 0.5;
    if params.flags.w != 0u {
        let homogeneous = vec3(uv, 1.0);
        let divisor = dot(params.points[2].xyz, homogeneous);
        uv = vec2(dot(params.points[0].xyz, homogeneous), dot(params.points[1].xyz, homogeneous)) / divisor;
    }
    var src = vec4(0.0);
    if params.appearance.y > 0.0 {
        src = sample_motion_blur(uv);
    } else {
        src = sample_source(uv);
    }
    src.a *= amount;
    let alpha = src.a + dst.a * (1.0 - src.a);
    let color = ((1.0 - src.a) * dst.a * dst.rgb + (1.0 - dst.a) * src.a * src.rgb
        + dst.a * src.a * blend(dst.rgb, src.rgb, params.flags.x)) / max(alpha, 0.000001);
    textureStore(output, position, vec4(color, alpha));
}
