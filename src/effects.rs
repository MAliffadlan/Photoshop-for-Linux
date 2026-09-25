use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Result, ensure};
use image::{Rgba, RgbaImage};
use rayon::prelude::*;

use crate::{
    document::{Adjustment, Document, LayerEffects, Mask, Point, Transform},
    paint::ensure_pixels,
    render, selection,
};

pub fn rgb_to_hsl(c: [f32; 3]) -> [f32; 3] {
    let high = c.into_iter().fold(f32::MIN, f32::max);
    let low = c.into_iter().fold(f32::MAX, f32::min);
    let light = (high + low) * 0.5;
    let delta = high - low;
    if delta < 1e-6 {
        return [0.0, 0.0, light];
    }
    let sat = delta / (1.0 - (2.0 * light - 1.0).abs()).max(1e-6);
    let hue = if high == c[0] {
        (c[1] - c[2]) / delta
    } else if high == c[1] {
        (c[2] - c[0]) / delta + 2.0
    } else {
        (c[0] - c[1]) / delta + 4.0
    };
    [(hue * 60.0).rem_euclid(360.0), sat, light]
}

pub fn hsl_to_rgb(hsl: [f32; 3]) -> [f32; 3] {
    let [h, s, l] = hsl;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let rgb = match h as u32 {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    rgb.map(|v| v + l - c * 0.5)
}

/// How much a tone belongs to the shadows, midtones and highlights: three overlapping curves that
/// sum to about one across the range, so a shift fades in and out instead of banding at a threshold.
/// These are Compositor's constants.
fn tonal_weights(value: f32) -> [f32; 3] {
    const RAMP: f32 = 0.25;
    const WIDTH: f32 = 0.333;
    const SCALE: f32 = 0.7;
    let shadow = ((value - WIDTH) / -RAMP + 0.5).clamp(0.0, 1.0) * SCALE;
    let highlight = ((value + WIDTH - 1.0) / RAMP + 0.5).clamp(0.0, 1.0) * SCALE;
    let rise = ((value - WIDTH) / RAMP + 0.5).clamp(0.0, 1.0);
    let fall = ((value + WIDTH - 1.0) / -RAMP + 0.5).clamp(0.0, 1.0);
    [shadow, rise * fall * SCALE, highlight]
}

pub fn curve_value(points: &[Point], value: f32) -> f32 {
    if points.len() < 2 {
        return value;
    }
    let i = points
        .partition_point(|p| p.x <= value)
        .saturating_sub(1)
        .min(points.len() - 2);
    let slope =
        |j: usize| (points[j + 1].y - points[j].y) / (points[j + 1].x - points[j].x).max(1e-6);
    let tangent = |j: usize| {
        if j == 0 {
            return slope(0);
        }
        if j == points.len() - 1 {
            return slope(j - 1);
        }
        let a = slope(j - 1);
        let b = slope(j);
        if a * b <= 0.0 {
            0.0
        } else {
            2.0 / (1.0 / a + 1.0 / b)
        }
    };
    let h = (points[i + 1].x - points[i].x).max(1e-6);
    let t = ((value - points[i].x) / h).clamp(0.0, 1.0);
    let t2 = t * t;
    let t3 = t2 * t;
    ((2.0 * t3 - 3.0 * t2 + 1.0) * points[i].y
        + (t3 - 2.0 * t2 + t) * h * tangent(i)
        + (-2.0 * t3 + 3.0 * t2) * points[i + 1].y
        + (t3 - t2) * h * tangent(i + 1))
    .clamp(0.0, 1.0)
}

/// Compositor's noise hash, which mixes a value into a value nothing like it.
fn noise_hash(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^= value >> 16;
    value
}

/// Uniform in [0, 1).
fn noise_unit(key: u32) -> f32 {
    (noise_hash(key) >> 8) as f32 / 16_777_216.0
}

/// A pixel's noise key: the seed, the pixel and the channel, mixed as the source mixes them.
fn noise_key(seed: u32, point: Point) -> u32 {
    let x = point.x.max(0.0) as u32;
    let y = point.y.max(0.0) as u32;
    noise_hash(
        seed ^ noise_hash(
            x.wrapping_mul(0x9e37_79b9)
                .wrapping_add(y.wrapping_mul(0x85eb_ca6b)),
        ),
    )
}

fn noise(x: u32, y: u32, seed: u32) -> f32 {
    let mut value = x
        .wrapping_mul(374761393)
        .wrapping_add(y.wrapping_mul(668265263))
        .wrapping_add(seed);
    value = (value ^ (value >> 13)).wrapping_mul(1274126177);
    value ^= value >> 16;
    value as f32 / u32::MAX as f32 * 2.0 - 1.0
}

fn hue_saturation(rgb: [f32; 3], adjustment: [f32; 3], colorize: bool) -> [f32; 3] {
    let [hue, saturation, lightness] = adjustment;
    let mut hsl = rgb_to_hsl(rgb);
    hsl[0] = if colorize { hue } else { hsl[0] + hue };
    hsl[1] = if colorize {
        saturation / 100.0
    } else {
        hsl[1] * (1.0 + saturation / 100.0)
    }
    .clamp(0.0, 1.0);
    let amount = (lightness / 100.0).clamp(-1.0, 1.0);
    hsl[2] = if amount < 0.0 {
        hsl[2] * (1.0 + amount)
    } else {
        hsl[2] + (1.0 - hsl[2]) * amount
    };
    hsl_to_rgb(hsl)
}

fn mix32(mut value: u32) -> u32 {
    value = (value ^ (value >> 16)).wrapping_mul(0x7feb352d);
    value = (value ^ (value >> 15)).wrapping_mul(0x846ca68b);
    value ^ (value >> 16)
}

fn lattice(x: i32, y: i32, seed: u32) -> f32 {
    let hash = mix32(
        (x as u32).wrapping_mul(0x9e3779b1) ^ mix32((y as u32).wrapping_mul(0x85ebca77) ^ seed),
    );
    (hash & 65535) as f32 / 65535.0 + (hash >> 16) as f32 / 65535.0 - 1.0
}

fn film_grain(point: Point, size: f32, roughness: f32, seed: u32) -> f32 {
    let x = point.x / size;
    let y = point.y / size;
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let smoothstep = |v: f32| v * v * (3.0 - 2.0 * v);
    let tx = smoothstep(x - x.floor());
    let ty = smoothstep(y - y.floor());
    let top = lattice(ix, iy, seed) * (1.0 - tx) + lattice(ix + 1, iy, seed) * tx;
    let bottom = lattice(ix, iy + 1, seed) * (1.0 - tx) + lattice(ix + 1, iy + 1, seed) * tx;
    let smooth = (top * (1.0 - ty) + bottom * ty) * 1.6;
    let fine = lattice(
        point.x.floor() as i32,
        point.y.floor() as i32,
        mix32(seed ^ 0xa511e9b3),
    );
    smooth + (fine - smooth) * roughness / 100.0
}

pub fn adjust(pixel: [f32; 4], adjustment: &Adjustment, point: Point) -> [f32; 4] {
    let rgb = [pixel[0], pixel[1], pixel[2]];
    let rgb = match adjustment {
        Adjustment::HueRanges { settings } => {
            let response = settings.response(rgb_to_hsl(rgb)[0]);
            hue_saturation(rgb, response, settings.colorize)
        }
        Adjustment::LevelsChannels { ranges } => std::array::from_fn(|i| {
            crate::color::level(crate::color::level(rgb[i], ranges[i + 1]), ranges[0])
        }),
        Adjustment::CurvesChannels { channels } => std::array::from_fn(|i| {
            curve_value(&channels[0], curve_value(&channels[i + 1], rgb[i]))
        }),
        Adjustment::HueSaturation {
            hue,
            saturation,
            lightness,
            colorize,
        } => hue_saturation(rgb, [*hue, *saturation, *lightness], *colorize),
        Adjustment::Levels {
            black,
            gamma,
            white,
            output_black,
            output_white,
        } => rgb.map(|v| {
            let v = ((v * 255.0 - black) / (white - black).max(1.0))
                .clamp(0.0, 1.0)
                .powf(1.0 / gamma.max(0.01));
            (output_black + v * (output_white - output_black)) / 255.0
        }),
        Adjustment::Curves { points } => rgb.map(|v| curve_value(points, v)),
        Adjustment::Exposure {
            exposure,
            offset,
            gamma,
        } => rgb.map(|v| {
            crate::color::encode_srgb(
                (crate::color::decode_srgb(v) * 2.0_f32.powf(*exposure) + offset)
                    .max(0.0)
                    .powf(1.0 / gamma.max(0.01)),
            )
        }),
        Adjustment::GradientMap {
            shadows,
            highlights,
        } => {
            let luma = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
            std::array::from_fn(|i| {
                (shadows[i] as f32 * (1.0 - luma) + highlights[i] as f32 * luma) / 255.0
            })
        }
        Adjustment::FilmGrain {
            amount,
            size,
            roughness,
            seed,
        } => {
            let level = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
            let delta = film_grain(point, *size, *roughness, *seed) * amount / 100.0
                * 0.35
                * (0.4 + 2.4 * level * (1.0 - level));
            rgb.map(|v| v + delta)
        }
        Adjustment::Grain {
            amount,
            monochrome,
            seed,
        } => std::array::from_fn(|i| {
            rgb[i]
                + noise(
                    point.x as u32,
                    point.y as u32,
                    seed.wrapping_add(if *monochrome { 0 } else { i as u32 * 12345 }),
                ) * amount
                    / 100.0
        }),
        Adjustment::AddNoise {
            amount,
            gaussian,
            monochromatic,
            seed,
        } => {
            // Compositor's own noise kernel: one hash per pixel, either a flat spread or a bell curve,
            // scaled so 100 spans 127.5 steps of the 255.
            let spread = amount / 100.0 * 127.5 / 255.0;
            let base = noise_key(*seed, point);
            std::array::from_fn(|i| {
                let key = if *monochromatic {
                    base
                } else {
                    base.wrapping_add(0x9e37_79b9_u32.wrapping_mul(i as u32))
                };
                let delta = if *gaussian {
                    // Box–Muller: two uniform values make one normally distributed one.
                    let u1 = noise_unit(key);
                    let u2 = noise_unit(key ^ 0x68e3_1da4);
                    (-2.0 * (1.0 - u1).ln()).sqrt()
                        * (std::f32::consts::TAU * u2).cos()
                        * spread
                        * (2.0 / 3.0)
                } else {
                    (noise_unit(key) * 2.0 - 1.0) * spread
                };
                rgb[i] + delta
            })
        }
        // A blur redraws the backdrop instead of the pixel it lands on. The compositor runs it before
        // the layers above are drawn, so there is nothing left to do by the time a pixel is reached.
        Adjustment::GaussianBlur { .. } | Adjustment::MotionBlur { .. } => rgb,
        Adjustment::Invert => rgb.map(|v| 1.0 - v),
        Adjustment::BlackWhite {
            reds,
            yellows,
            greens,
            cyans,
            blues,
            magentas,
            tint,
            tint_hue,
            tint_saturation,
        } => {
            let weights = [*reds, *yellows, *greens, *cyans, *blues, *magentas].map(|w| w / 100.0);
            // The two strongest channels decide which families of color this pixel sits between;
            // the middle one mixes them, exactly as Compositor's routine classifies it.
            let high = rgb[0].max(rgb[1]).max(rgb[2]);
            let low = rgb[0].min(rgb[1]).min(rgb[2]);
            let middle = rgb[0] + rgb[1] + rgb[2] - high - low;
            let (primary, secondary) = if high == rgb[0] {
                (0, if rgb[1] >= rgb[2] { 1 } else { 5 })
            } else if high == rgb[1] {
                (2, if rgb[0] >= rgb[2] { 1 } else { 3 })
            } else {
                (4, if rgb[1] >= rgb[0] { 3 } else { 5 })
            };
            let gray =
                (low + (middle - low) * weights[secondary] + (high - middle) * weights[primary])
                    .clamp(0.0, 1.0);
            if *tint && *tint_saturation > 0.0 {
                hsl_to_rgb([*tint_hue, (*tint_saturation / 100.0).clamp(0.0, 1.0), gray])
                    .map(|v| v.clamp(0.0, 1.0))
            } else {
                [gray; 3]
            }
        }
        Adjustment::ColorBalance {
            shadows,
            midtones,
            highlights,
            preserve_luminosity,
        } => {
            let before = rgb[0] * 0.299 + rgb[1] * 0.587 + rgb[2] * 0.114;
            let mut balanced = std::array::from_fn(|i| {
                let weights = tonal_weights(rgb[i]);
                (rgb[i]
                    + shadows[i] / 100.0 * weights[0]
                    + midtones[i] / 100.0 * weights[1]
                    + highlights[i] / 100.0 * weights[2])
                    .clamp(0.0, 1.0)
            });
            if *preserve_luminosity {
                let after = balanced[0] * 0.299 + balanced[1] * 0.587 + balanced[2] * 0.114;
                if after > 0.0001 {
                    let ratio = before / after;
                    balanced = balanced.map(|v| (v * ratio).clamp(0.0, 1.0));
                }
            }
            balanced
        }
    };
    [
        rgb[0].clamp(0.0, 1.0),
        rgb[1].clamp(0.0, 1.0),
        rgb[2].clamp(0.0, 1.0),
        pixel[3],
    ]
}

pub fn apply_adjustment(
    document: &mut Document,
    adjustment: &Adjustment,
    mask_target: bool,
) -> Result<()> {
    // Applied straight to a layer, a blur is the filter of the same name, on the whole image rather
    // than on the backdrop: there is nothing underneath to redraw.
    let blur = match adjustment {
        Adjustment::GaussianBlur { radius } => Some(Filter::GaussianBlur { radius: *radius }),
        Adjustment::MotionBlur { angle, distance } => Some(Filter::MotionBlur {
            distance: *distance,
            angle: *angle,
        }),
        _ => None,
    };
    if let Some(filter) = blur {
        return apply_filter(document, &filter, mask_target);
    }
    let selection = document.selection.clone();
    let layer = document
        .active_mut()
        .ok_or_else(|| anyhow::anyhow!("Select a layer first"))?;
    if mask_target {
        crate::paint::prepare_mask(layer)?;
        let transform = layer
            .mask
            .as_ref()
            .and_then(|m| m.placement)
            .unwrap_or(layer.transform);
        if let Some(result) = crate::gpu::adjust_mask(
            &layer.mask.as_ref().unwrap().pixels,
            adjustment,
            transform,
            selection.as_deref(),
        ) {
            layer.mask.as_mut().unwrap().pixels = Arc::new(result);
            return Ok(());
        }
        let pixels = Arc::make_mut(&mut layer.mask.as_mut().unwrap().pixels);
        let (w, h) = pixels.dimensions();
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            let point = transform.point(Point::new(
                (x as f32 + 0.5) / w as f32,
                (y as f32 + 0.5) / h as f32,
            ));
            let amount = selection::coverage(selection.as_deref(), point);
            let old = pixel[0] as f32 / 255.0;
            let new = adjust([old, old, old, 1.0], adjustment, point)[0];
            pixel[0] = ((old * (1.0 - amount) + new * amount) * 255.0).round() as u8;
        }
        return Ok(());
    }
    ensure_pixels(layer)?;
    let transform = layer.transform;
    if let Some(result) = crate::gpu::adjustment(
        layer.pixels.as_ref().unwrap(),
        adjustment,
        transform,
        selection.as_deref(),
    ) {
        layer.pixels = Some(Arc::new(result));
        return Ok(());
    }
    let pixels = Arc::make_mut(layer.pixels.as_mut().unwrap());
    let (w, h) = pixels.dimensions();
    pixels
        .as_mut()
        .par_chunks_exact_mut(4)
        .enumerate()
        .for_each(|(index, pixel)| {
            let point = transform.point(Point::new(
                ((index as u32 % w) as f32 + 0.5) / w as f32,
                ((index as u32 / w) as f32 + 0.5) / h as f32,
            ));
            let amount = selection::coverage(selection.as_deref(), point);
            let old = [pixel[0], pixel[1], pixel[2], pixel[3]].map(|v| v as f32 / 255.0);
            let new = adjust(old, adjustment, point);
            for i in 0..3 {
                pixel[i] = ((old[i] * (1.0 - amount) + new[i] * amount) * 255.0).round() as u8;
            }
        });
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    GaussianBlur {
        radius: f32,
    },
    MotionBlur {
        distance: f32,
        angle: f32,
    },
    Noise {
        amount: f32,
        monochrome: bool,
    },
    Vignette {
        amount: f32,
        color: [f32; 3],
        midpoint: f32,
        roundness: f32,
        feather: f32,
        highlights: f32,
    },
    BloomGlow {
        amount: f32,
        radius: f32,
    },
    TonalContrast {
        amount: f32,
        radius: f32,
        shadows: f32,
        midtones: f32,
        highlights: f32,
    },
    LensCorrection {
        distortion: f32,
        vignette: f32,
    },
}

impl Filter {
    pub fn name(&self) -> &'static str {
        match self {
            Self::GaussianBlur { .. } => "Gaussian Blur",
            Self::MotionBlur { .. } => "Motion Blur",
            Self::Noise { .. } => "Add Noise",
            Self::Vignette { .. } => "Vignette",
            Self::BloomGlow { .. } => "Bloom / Glow",
            Self::TonalContrast { .. } => "Tonal Contrast",
            Self::LensCorrection { .. } => "Lens Correction",
        }
    }
}

pub fn filtered(image: &RgbaImage, filter: &Filter) -> RgbaImage {
    if let Some(result) = crate::gpu::filter(image, filter) {
        return result;
    }
    if crate::gpu::cancelled() {
        return image.clone();
    }
    filtered_cpu(image, filter, false)
}

fn premultiplied_blur(image: &RgbaImage, radius: f32) -> RgbaImage {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return image.clone();
    }
    let premultiplied = RgbaImage::from_fn(width, height, |x, y| {
        let p = image.get_pixel(x, y).0;
        Rgba([
            ((p[0] as u16 * p[3] as u16) / 255) as u8,
            ((p[1] as u16 * p[3] as u16) / 255) as u8,
            ((p[2] as u16 * p[3] as u16) / 255) as u8,
            p[3],
        ])
    });
    let mut result = image::imageops::blur(&premultiplied, radius.max(0.01));
    for pixel in result.pixels_mut() {
        if pixel[3] > 0 {
            for i in 0..3 {
                pixel[i] = ((pixel[i] as u32 * 255) / pixel[3] as u32).min(255) as u8;
            }
        }
    }
    result
}

fn vignette_mask_at(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    midpoint: f32,
    roundness: f32,
    feather: f32,
) -> f32 {
    let nx = x / width * 2.0 - 1.0;
    let ny = y / height * 2.0 - 1.0;
    let square = nx.abs().max(ny.abs());
    let circle = (nx * nx + ny * ny).sqrt() / 2.0_f32.sqrt();
    let shape = (1.0 - roundness / 100.0) * 0.5;
    let distance = circle + (square - circle) * shape;
    let start = midpoint / 100.0 * 0.85;
    let softness = (feather / 100.0).max(0.05);
    let t = ((distance - start) / softness).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn vignette(image: &RgbaImage, filter: &Filter, fill_empty: bool) -> RgbaImage {
    let Filter::Vignette {
        amount,
        color,
        midpoint,
        roundness,
        feather,
        highlights,
    } = filter
    else {
        return image.clone();
    };
    let amount = *amount;
    let color = *color;
    let midpoint = *midpoint;
    let roundness = *roundness;
    let feather = *feather;
    let highlights = *highlights;
    let (width, height) = image.dimensions();
    if amount <= 0.0 || width == 0 || height == 0 {
        return image.clone();
    }
    let color = color.map(|value| value.clamp(0.0, 1.0));
    let strength = (amount / 100.0).clamp(0.0, 1.0);
    RgbaImage::from_fn(width, height, |x, y| {
        let pixel = *image.get_pixel(x, y);
        let alpha = pixel[3] as f32 / 255.0;
        if alpha <= 0.0 && !fill_empty {
            return pixel;
        }
        let mask = vignette_mask_at(
            x as f32 + 0.5,
            y as f32 + 0.5,
            width as f32,
            height as f32,
            midpoint,
            roundness,
            feather,
        );
        if mask <= 0.0 {
            return pixel;
        }
        let rgb = if alpha > 0.0 {
            [
                (pixel[0] as f32 / 255.0).min(1.0),
                (pixel[1] as f32 / 255.0).min(1.0),
                (pixel[2] as f32 / 255.0).min(1.0),
            ]
        } else {
            [0.0; 3]
        };
        let luminance = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        let bright = ((luminance - 0.45) / 0.55).clamp(0.0, 1.0);
        let effect = strength * mask * (1.0 - highlights / 100.0 * bright);
        let (out_rgb, out_alpha) = if fill_empty {
            let out_alpha = (alpha + effect * (1.0 - alpha)).clamp(0.0, 1.0);
            let out = if out_alpha > 0.0 {
                [
                    (color[0] * effect + rgb[0] * alpha * (1.0 - effect)) / out_alpha,
                    (color[1] * effect + rgb[1] * alpha * (1.0 - effect)) / out_alpha,
                    (color[2] * effect + rgb[2] * alpha * (1.0 - effect)) / out_alpha,
                ]
            } else {
                [0.0; 3]
            };
            (out, out_alpha)
        } else {
            (
                [
                    rgb[0] + (color[0] - rgb[0]) * effect,
                    rgb[1] + (color[1] - rgb[1]) * effect,
                    rgb[2] + (color[2] - rgb[2]) * effect,
                ],
                alpha,
            )
        };
        Rgba([
            (out_rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (out_rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (out_rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            (out_alpha * 255.0).round() as u8,
        ])
    })
}

fn bloom(image: &RgbaImage, amount: f32, radius: f32) -> RgbaImage {
    if amount <= 0.0 || image.width() == 0 || image.height() == 0 {
        return image.clone();
    }
    let (width, height) = image.dimensions();
    let highlights = RgbaImage::from_fn(width, height, |x, y| {
        let pixel = *image.get_pixel(x, y);
        let alpha = pixel[3] as f32 / 255.0;
        if alpha <= 0.0 {
            return Rgba([0; 4]);
        }
        let rgb = [
            (pixel[0] as f32 / 255.0).min(1.0),
            (pixel[1] as f32 / 255.0).min(1.0),
            (pixel[2] as f32 / 255.0).min(1.0),
        ];
        let luminance = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        let highlight = ((luminance - 0.5) / 0.5).clamp(0.0, 1.0);
        Rgba([
            (rgb[0] * highlight * 255.0).round() as u8,
            (rgb[1] * highlight * 255.0).round() as u8,
            (rgb[2] * highlight * 255.0).round() as u8,
            (alpha * highlight * 255.0).round() as u8,
        ])
    });
    let blurred = premultiplied_blur(&highlights, radius);
    let intensity = amount / 50.0;
    RgbaImage::from_fn(width, height, |x, y| {
        let source = *image.get_pixel(x, y);
        let glow = *blurred.get_pixel(x, y);
        let source_alpha = source[3] as f32 / 255.0;
        let glow_alpha = glow[3] as f32 / 255.0 * intensity;
        let glow_rgb = [
            glow[0] as f32 / 255.0,
            glow[1] as f32 / 255.0,
            glow[2] as f32 / 255.0,
        ];
        let out_alpha = (source_alpha + glow_alpha).clamp(0.0, 1.0);
        if out_alpha <= 0.0 {
            return source;
        }
        let out_rgb = [
            (source[0] as f32 / 255.0 * source_alpha + glow_rgb[0] * glow_alpha) / out_alpha,
            (source[1] as f32 / 255.0 * source_alpha + glow_rgb[1] * glow_alpha) / out_alpha,
            (source[2] as f32 / 255.0 * source_alpha + glow_rgb[2] * glow_alpha) / out_alpha,
        ];
        Rgba([
            (out_rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (out_rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (out_rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            (out_alpha * 255.0).round() as u8,
        ])
    })
}

fn tonal_contrast(
    image: &RgbaImage,
    amount: f32,
    radius: f32,
    shadows: f32,
    midtones: f32,
    highlights: f32,
) -> RgbaImage {
    if amount <= 0.0
        || (shadows == 0.0 && midtones == 0.0 && highlights == 0.0)
        || image.width() == 0
        || image.height() == 0
    {
        return image.clone();
    }
    let base = premultiplied_blur(image, radius);
    let strength = amount / 50.0;
    let smooth = |low: f32, high: f32, value: f32| {
        let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let (width, height) = image.dimensions();
    RgbaImage::from_fn(width, height, |x, y| {
        let pixel = *image.get_pixel(x, y);
        let alpha = pixel[3] as f32 / 255.0;
        let base_pixel = *base.get_pixel(x, y);
        let base_alpha = base_pixel[3] as f32 / 255.0;
        if alpha <= 0.0 || base_alpha <= 0.0 {
            return pixel;
        }
        let rgb = [
            (pixel[0] as f32 / 255.0).min(1.0),
            (pixel[1] as f32 / 255.0).min(1.0),
            (pixel[2] as f32 / 255.0).min(1.0),
        ];
        let base_rgb = [
            base_pixel[0] as f32 / 255.0,
            base_pixel[1] as f32 / 255.0,
            base_pixel[2] as f32 / 255.0,
        ];
        let luminance = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        let base_luminance = base_rgb[0] * 0.2126 + base_rgb[1] * 0.7152 + base_rgb[2] * 0.0722;
        let shadow_weight = 1.0 - smooth(0.15, 0.5, base_luminance);
        let highlight_weight = smooth(0.5, 0.85, base_luminance);
        let midtone_weight = 1.0 - shadow_weight - highlight_weight;
        let weight =
            (shadows * shadow_weight + midtones * midtone_weight + highlights * highlight_weight)
                / 100.0;
        let detail = luminance - base_luminance;
        let delta = 0.18
            * (detail * 6.0).tanh()
            * weight
            * strength
            * (4.0 * luminance * (1.0 - luminance));
        Rgba([
            ((rgb[0] + delta).clamp(0.0, 1.0) * 255.0).round() as u8,
            ((rgb[1] + delta).clamp(0.0, 1.0) * 255.0).round() as u8,
            ((rgb[2] + delta).clamp(0.0, 1.0) * 255.0).round() as u8,
            pixel[3],
        ])
    })
}

fn filtered_cpu(image: &RgbaImage, filter: &Filter, fill_empty_vignette: bool) -> RgbaImage {
    let (w, h) = image.dimensions();
    let result = match filter {
        Filter::GaussianBlur { radius } => premultiplied_blur(image, *radius),
        Filter::MotionBlur { distance, angle } => {
            motion_blur(image, *distance, *angle, &AtomicBool::new(false))
                .expect("Motion blur was not cancelled")
        }
        Filter::Noise { amount, monochrome } => RgbaImage::from_fn(w, h, |x, y| {
            Rgba(
                adjust(
                    image.get_pixel(x, y).0.map(|v| v as f32 / 255.0),
                    &Adjustment::Grain {
                        amount: *amount,
                        monochrome: *monochrome,
                        seed: 3187,
                    },
                    Point::new(x as f32, y as f32),
                )
                .map(|v| (v * 255.0).round() as u8),
            )
        }),
        Filter::Vignette { .. } => vignette(image, filter, fill_empty_vignette),
        Filter::BloomGlow { amount, radius } => bloom(image, *amount, *radius),
        Filter::TonalContrast {
            amount,
            radius,
            shadows,
            midtones,
            highlights,
        } => tonal_contrast(image, *amount, *radius, *shadows, *midtones, *highlights),
        Filter::LensCorrection {
            distortion,
            vignette,
        } => RgbaImage::from_fn(w, h, |x, y| {
            let u = (x as f32 + 0.5) / w as f32 * 2.0 - 1.0;
            let v = (y as f32 + 0.5) / h as f32 * 2.0 - 1.0;
            let radius = u * u + v * v;
            let k = 1.0 + distortion * radius / 100.0;
            let mut p = render::sample(image, Point::new((u * k + 1.0) * 0.5, (v * k + 1.0) * 0.5));
            for value in &mut p[..3] {
                *value *= 1.0 - vignette * radius * 0.005;
            }
            Rgba(p.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8))
        }),
    };
    if crate::gpu::cancelled() {
        image.clone()
    } else {
        result
    }
}

fn motion_blur(
    image: &RgbaImage,
    distance: f32,
    angle: f32,
    cancel: &AtomicBool,
) -> Result<RgbaImage> {
    ensure!(!cancel.load(Ordering::Relaxed), "Filter cancelled");
    let (width, height) = image.dimensions();
    let mut result = RgbaImage::new(width, height);
    if width == 0 || height == 0 {
        return Ok(result);
    }
    let steps = distance.ceil().clamp(1.0, 256.0) as u32;
    let (sin, cos) = angle.to_radians().sin_cos();
    // Translation gives every pixel the same bilinear weights. Compute them once,
    // and accumulate premultiplied colors without unpremultiplying every sample.
    let samples: Vec<_> = (0..steps)
        .map(|i| {
            let offset = ((i as f32 + 0.5) / steps as f32 - 0.5) * distance;
            let (x, y) = (offset * cos, offset * sin);
            let (fx, fy) = (x - x.floor(), y - y.floor());
            (
                x,
                y,
                x.floor() as i32,
                y.floor() as i32,
                [
                    (1.0 - fx) * (1.0 - fy),
                    fx * (1.0 - fy),
                    (1.0 - fx) * fy,
                    fx * fy,
                ],
            )
        })
        .collect();
    result
        .as_mut()
        .par_chunks_exact_mut(width as usize * 4)
        .enumerate()
        .try_for_each(|(y, row)| -> Result<()> {
            ensure!(!cancel.load(Ordering::Relaxed), "Filter cancelled");
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let mut sum = [0.0; 4];
                for &(ox, oy, dx, dy, weights) in &samples {
                    let sx = x as f32 + 0.5 + ox;
                    let sy = y as f32 + 0.5 + oy;
                    if sx < 0.0 || sx >= width as f32 || sy < 0.0 || sy >= height as f32 {
                        continue;
                    }
                    for (i, weight) in weights.into_iter().enumerate() {
                        if weight == 0.0 {
                            continue;
                        }
                        let px = (x as i32 + dx + (i % 2) as i32).clamp(0, width as i32 - 1);
                        let py = (y as i32 + dy + (i / 2) as i32).clamp(0, height as i32 - 1);
                        let p = image.get_pixel(px as u32, py as u32);
                        let alpha = p[3] as f32 * weight;
                        for c in 0..3 {
                            sum[c] += p[c] as f32 * alpha;
                        }
                        sum[3] += alpha;
                    }
                }
                if sum[3] > 0.0 {
                    for c in 0..3 {
                        pixel[c] = (sum[c] / sum[3]).round() as u8;
                    }
                }
                pixel[3] = (sum[3] / steps as f32).round() as u8;
            }
            Ok(())
        })?;
    Ok(result)
}

pub fn apply_filter(document: &mut Document, filter: &Filter, mask_target: bool) -> Result<()> {
    apply_filter_cancellable(document, filter, mask_target, &AtomicBool::new(false))
}

pub fn apply_filter_cancellable(
    document: &mut Document,
    filter: &Filter,
    mask_target: bool,
    cancel: &AtomicBool,
) -> Result<()> {
    let processor = crate::gpu::current();
    let gpu = processor
        .as_ref()
        .filter(|_| {
            document
                .active()
                .and_then(|l| l.pixels.as_ref())
                .is_some_and(|p| u64::from(p.width()) * u64::from(p.height()) >= 16_384)
        })
        .map(|p| &p.motion_blur);
    apply_filter_impl(document, filter, mask_target, cancel, gpu)
}

/// Use the GPU for full-resolution Motion Blur, with CPU fallback for device
/// limits or GPU failures. Selection coverage and document edits are shared.
pub fn apply_filter_with_gpu(
    document: &mut Document,
    filter: &Filter,
    mask_target: bool,
    cancel: &AtomicBool,
    gpu: &crate::gpu::GpuMotionBlur,
) -> Result<()> {
    apply_filter_impl(document, filter, mask_target, cancel, Some(gpu))
}

fn apply_filter_impl(
    document: &mut Document,
    filter: &Filter,
    mask_target: bool,
    cancel: &AtomicBool,
    gpu: Option<&crate::gpu::GpuMotionBlur>,
) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "Filter cancelled");
    let selection = document.selection.clone();
    let layer = document
        .active_mut()
        .ok_or_else(|| anyhow::anyhow!("Select a layer first"))?;
    let empty_layer = layer.pixels.is_none();
    if mask_target {
        crate::paint::prepare_mask(layer)?;
        let mask = layer.mask.as_mut().unwrap();
        if let Filter::GaussianBlur { radius } = filter {
            let mut result = crate::gpu::blur_gray(&mask.pixels, radius.max(0.01));
            ensure!(!cancel.load(Ordering::Relaxed), "Filter cancelled");
            let transform = mask.placement.unwrap_or(layer.transform);
            let (width, height) = result.dimensions();
            if selection.is_none() {
                mask.pixels = Arc::new(result);
                return Ok(());
            }
            if let Some(bytes) = crate::gpu::filter_selection(crate::gpu::FilterSelection {
                image: result.as_raw(),
                original: mask.pixels.as_raw(),
                size: [width, height],
                original_size: [width, height],
                transform,
                selection: selection.as_deref().unwrap(),
                padding: 0,
                mask: true,
            }) {
                ensure!(!cancel.load(Ordering::Relaxed), "Filter cancelled");
                mask.pixels = Arc::new(image::GrayImage::from_raw(width, height, bytes).unwrap());
                return Ok(());
            }
            for (x, y, pixel) in result.enumerate_pixels_mut() {
                if x == 0 {
                    ensure!(!cancel.load(Ordering::Relaxed), "Filter cancelled");
                }
                let point = transform.point(Point::new(
                    (x as f32 + 0.5) / width as f32,
                    (y as f32 + 0.5) / height as f32,
                ));
                let amount = selection::coverage(selection.as_deref(), point);
                pixel[0] = (mask.pixels.get_pixel(x, y)[0] as f32 * (1.0 - amount)
                    + pixel[0] as f32 * amount)
                    .round() as u8;
            }
            mask.pixels = Arc::new(result);
            return Ok(());
        }
        anyhow::bail!("Use Gaussian Blur on a mask");
    }
    ensure_pixels(layer)?;
    let original_transform = layer.transform;
    let original = layer.pixels.as_ref().unwrap();
    let padding = match filter {
        Filter::GaussianBlur { radius } => (radius * 3.0).ceil() as u32,
        Filter::MotionBlur { distance, .. } => (distance * 0.5).ceil() as u32 + 1,
        Filter::BloomGlow { radius, .. } => (radius * 3.0).ceil() as u32 + 2,
        _ => 0,
    };
    let (w, h) = (
        original.width() + padding * 2,
        original.height() + padding * 2,
    );
    crate::document::validate_size(w, h)?;
    let mut transform = original_transform;
    if padding > 0 {
        let width = original.width() as f32;
        let height = original.height() as f32;
        let pad = padding as f32;
        transform = original_transform.expanded(
            -pad / width,
            -pad / height,
            1.0 + pad / width,
            1.0 + pad / height,
        );
    }
    ensure!(!cancel.load(Ordering::Relaxed), "Filter cancelled");
    let accelerated = if let (Some(gpu), Filter::MotionBlur { distance, angle }) = (gpu, filter) {
        // WGPU can report allocation/validation failures through its panic handler.
        // An unsuccessful GPU attempt must not discard the user's pending edit.
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gpu.render(original, *distance, *angle, padding, cancel)
        }))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("GPU filter failed unexpectedly")));
        ensure!(!cancel.load(Ordering::Relaxed), "Filter cancelled");
        match attempt {
            Ok(result) => result,
            Err(error) => {
                eprintln!("GPU Motion Blur unavailable, using CPU: {error:#}");
                None
            }
        }
    } else {
        None
    };
    let mut result = if let Some(result) = accelerated {
        result
    } else {
        let mut expanded = RgbaImage::new(w, h);
        image::imageops::replace(&mut expanded, &**original, padding as i64, padding as i64);
        match filter {
            Filter::MotionBlur { distance, angle } => {
                motion_blur(&expanded, *distance, *angle, cancel)?
            }
            Filter::Vignette { .. } if empty_layer => filtered_cpu(&expanded, filter, true),
            _ => filtered(&expanded, filter),
        }
    };
    ensure!(!cancel.load(Ordering::Relaxed), "Filter cancelled");
    let blended = selection.as_deref().and_then(|selection| {
        crate::gpu::filter_selection(crate::gpu::FilterSelection {
            image: result.as_raw(),
            original: original.as_raw(),
            size: [w, h],
            original_size: [original.width(), original.height()],
            transform,
            selection,
            padding,
            mask: false,
        })
    });
    ensure!(!cancel.load(Ordering::Relaxed), "Filter cancelled");
    if let Some(bytes) = blended {
        result = RgbaImage::from_raw(w, h, bytes).unwrap();
    } else if selection.is_some() {
        for (x, y, pixel) in result.enumerate_pixels_mut() {
            if x == 0 {
                ensure!(!cancel.load(Ordering::Relaxed), "Filter cancelled");
            }
            let point = transform.point(Point::new(
                (x as f32 + 0.5) / w as f32,
                (y as f32 + 0.5) / h as f32,
            ));
            let amount = selection::coverage(selection.as_deref(), point);
            let old = if (padding..padding + original.width()).contains(&x)
                && (padding..padding + original.height()).contains(&y)
            {
                original.get_pixel(x - padding, y - padding).0
            } else {
                [0; 4]
            };
            for i in 0..4 {
                pixel[i] =
                    (old[i] as f32 * (1.0 - amount) + pixel[i] as f32 * amount).round() as u8;
            }
        }
    }
    if padding > 0
        && let Some(mask) = &mut layer.mask
    {
        mask.placement = Some(mask.placement.unwrap_or(original_transform));
    }
    layer.transform = transform;
    layer.pixels = Some(Arc::new(result));
    Ok(())
}

pub fn histogram(image: &RgbaImage) -> [u32; 256] {
    // This integer reduction is faster than a GPU upload/readback on CPU-owned
    // pixels. RAW preview combines RGB histograms and warnings in one GPU pass.
    let mut bins = [0; 256];
    for pixel in image.pixels().filter(|p| p[3] != 0) {
        // Integer weights make half-bin rounding identical on CPU and GPU.
        let value = (u32::from(pixel[0]) * 2126
            + u32::from(pixel[1]) * 7152
            + u32::from(pixel[2]) * 722
            + 5000) as usize
            / 10000;
        bins[value.min(255)] += 1;
    }
    bins
}

pub fn auto_levels(image: &RgbaImage) -> Adjustment {
    let bins = histogram(image);
    let total: u32 = bins.iter().sum();
    let cutoff = total / 200;
    let mut sum = 0;
    let black = bins
        .iter()
        .position(|n| {
            sum += n;
            sum > cutoff
        })
        .unwrap_or(0) as f32;
    sum = 0;
    let white = 255
        - bins
            .iter()
            .rev()
            .position(|n| {
                sum += n;
                sum > cutoff
            })
            .unwrap_or(0);
    Adjustment::Levels {
        black: black.min(254.0),
        white: (white as f32).max(black + 1.0),
        gamma: 1.0,
        output_black: 0.0,
        output_white: 255.0,
    }
}

pub fn validate_adjustment(adjustment: &Adjustment) -> Result<()> {
    let valid = match adjustment {
        Adjustment::HueRanges { settings } => settings.valid(),
        Adjustment::LevelsChannels { ranges } => ranges.iter().all(|r| {
            validate_adjustment(&Adjustment::Levels {
                black: r[0],
                gamma: r[1],
                white: r[2],
                output_black: r[3],
                output_white: r[4],
            })
            .is_ok()
        }),
        Adjustment::CurvesChannels { channels } => channels.iter().all(|points| {
            validate_adjustment(&Adjustment::Curves {
                points: points.clone(),
            })
            .is_ok()
        }),
        Adjustment::HueSaturation {
            hue,
            saturation,
            lightness,
            ..
        } => {
            hue.is_finite()
                && hue.abs() <= 360.0
                && saturation.is_finite()
                && saturation.abs() <= 100.0
                && lightness.is_finite()
                && lightness.abs() <= 100.0
        }
        Adjustment::Levels {
            black,
            gamma,
            white,
            output_black,
            output_white,
        } => {
            [black, gamma, white, output_black, output_white]
                .iter()
                .all(|v| v.is_finite())
                && *white > *black
                && *black >= 0.0
                && *white <= 255.0
                && (0.01..=10.0).contains(gamma)
                && (0.0..=255.0).contains(output_black)
                && (0.0..=255.0).contains(output_white)
        }
        Adjustment::Curves { points } => {
            (2..=32).contains(&points.len())
                && points.first().is_some_and(|p| p.x == 0.0)
                && points.last().is_some_and(|p| p.x == 1.0)
                && points
                    .iter()
                    .all(|p| p.x.is_finite() && p.y.is_finite() && (0.0..=1.0).contains(&p.y))
                && points.windows(2).all(|p| p[0].x < p[1].x)
        }
        Adjustment::Exposure {
            exposure,
            offset,
            gamma,
        } => {
            exposure.is_finite()
                && exposure.abs() <= 20.0
                && offset.is_finite()
                && offset.abs() <= 1.0
                && gamma.is_finite()
                && (0.01..=10.0).contains(gamma)
        }
        Adjustment::FilmGrain {
            amount,
            size,
            roughness,
            ..
        } => {
            amount.is_finite()
                && (0.0..=100.0).contains(amount)
                && size.is_finite()
                && (0.1..=100.0).contains(size)
                && roughness.is_finite()
                && (0.0..=100.0).contains(roughness)
        }
        Adjustment::Grain { amount, .. } => amount.is_finite() && (0.0..=100.0).contains(amount),
        // The ranges and defaults Compositor gives its blur and noise adjustments.
        Adjustment::AddNoise { amount, .. } => amount.is_finite() && (0.1..=400.0).contains(amount),
        Adjustment::GaussianBlur { radius } => radius.is_finite() && (0.1..=250.0).contains(radius),
        Adjustment::MotionBlur { angle, distance } => {
            angle.is_finite()
                && (-90.0..=90.0).contains(angle)
                && distance.is_finite()
                && (1.0..=2000.0).contains(distance)
        }
        Adjustment::GradientMap { .. } | Adjustment::Invert => true,
        // The ranges Compositor validates with: Photoshop's weight limits and the tint's HSL.
        Adjustment::BlackWhite {
            reds,
            yellows,
            greens,
            cyans,
            blues,
            magentas,
            tint_hue,
            tint_saturation,
            ..
        } => {
            [reds, yellows, greens, cyans, blues, magentas]
                .iter()
                .all(|weight| weight.is_finite() && (-200.0..=300.0).contains(*weight))
                && tint_hue.is_finite()
                && (0.0..=360.0).contains(tint_hue)
                && tint_saturation.is_finite()
                && (0.0..=100.0).contains(tint_saturation)
        }
        Adjustment::ColorBalance {
            shadows,
            midtones,
            highlights,
            ..
        } => [shadows, midtones, highlights].iter().all(|shift| {
            shift
                .iter()
                .all(|v| v.is_finite() && (-100.0..=100.0).contains(v))
        }),
    };
    ensure!(valid, "Invalid adjustment settings");
    Ok(())
}

fn effect_margin(effects: &LayerEffects) -> u32 {
    let mut reach = 0.0_f32;
    if let Some(effect) = effects
        .stroke
        .as_ref()
        .filter(|effect| effect.enabled && effect.size > 0.0 && effect.opacity > 0.0)
    {
        reach = reach.max(effect.size);
    }
    if let Some(effect) = effects
        .shadow
        .as_ref()
        .filter(|effect| effect.enabled && effect.opacity > 0.0)
    {
        reach = reach.max(effect.distance + effect.blur * 3.0);
    }
    if let Some(effect) = effects
        .outer_glow
        .as_ref()
        .filter(|effect| effect.enabled && effect.size > 0.0 && effect.opacity > 0.0)
    {
        reach = reach.max(effect.size * 3.0);
    }
    2 + reach.ceil().max(0.0) as u32
}

fn alpha_sample(alpha: &[f32], width: usize, height: usize, x: f32, y: f32) -> f32 {
    if width == 0 || height == 0 {
        return 0.0;
    }
    let x0 = x.floor() as isize;
    let y0 = y.floor() as isize;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let mut result = 0.0;
    for (iy, wy) in [(0, 1.0 - fy), (1, fy)] {
        for (ix, wx) in [(0, 1.0 - fx), (1, fx)] {
            let px = x0 + ix;
            let py = y0 + iy;
            if px < 0 || py < 0 || px >= width as isize || py >= height as isize {
                continue;
            }
            result += alpha[py as usize * width + px as usize] * wx * wy;
        }
    }
    result
}

fn shift_alpha(alpha: &[f32], width: usize, height: usize, dx: f32, dy: f32) -> Vec<f32> {
    let mut shifted = vec![0.0; alpha.len()];
    for y in 0..height {
        for x in 0..width {
            shifted[y * width + x] =
                alpha_sample(alpha, width, height, x as f32 - dx, y as f32 - dy);
        }
    }
    shifted
}

fn gaussian_alpha(alpha: &[f32], width: usize, height: usize, blur: f32) -> Vec<f32> {
    let sigma = blur * 0.5;
    if sigma <= 0.01 || width == 0 || height == 0 {
        return alpha.to_vec();
    }
    let radius = (blur * 1.5).round().max(1.0) as isize;
    let raw_weights: Vec<f32> = (-radius..=radius)
        .map(|offset| {
            let offset = offset as f32;
            (-(offset * offset) / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let weight_sum: f32 = raw_weights.iter().sum();
    let weights: Vec<f32> = raw_weights
        .into_iter()
        .map(|weight| weight / weight_sum)
        .collect();
    let mut horizontal = vec![0.0; alpha.len()];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0;
            for (index, weight) in weights.iter().enumerate() {
                let offset = index as isize - radius;
                let px = (x as isize + offset).clamp(0, width as isize - 1) as usize;
                sum += alpha[y * width + px] * weight;
            }
            horizontal[y * width + x] = sum;
        }
    }
    let mut result = vec![0.0; alpha.len()];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0;
            for (index, weight) in weights.iter().enumerate() {
                let offset = index as isize - radius;
                let py = (y as isize + offset).clamp(0, height as isize - 1) as usize;
                sum += horizontal[py * width + x] * weight;
            }
            result[y * width + x] = sum;
        }
    }
    result
}

fn morphology(
    alpha: &[f32],
    width: usize,
    height: usize,
    radius: usize,
    maximum: bool,
) -> Vec<f32> {
    if radius == 0 || width == 0 || height == 0 {
        return alpha.to_vec();
    }
    let radius = radius.min(width.max(height));
    let mut horizontal = vec![0.0; alpha.len()];
    for y in 0..height {
        morphology_line(alpha, &mut horizontal, y * width, 1, width, radius, maximum);
    }
    let mut result = vec![0.0; alpha.len()];
    for x in 0..width {
        morphology_line(&horizontal, &mut result, x, width, height, radius, maximum);
    }
    result
}

fn morphology_line(
    input: &[f32],
    output: &mut [f32],
    base: usize,
    stride: usize,
    length: usize,
    radius: usize,
    maximum: bool,
) {
    let value_at = |position: usize| {
        if position >= radius && position < radius + length {
            input[base + (position - radius) * stride]
        } else {
            0.0
        }
    };
    let mut queue = VecDeque::<usize>::with_capacity(radius * 2 + 1);
    for position in 0..length + radius * 2 {
        let value = value_at(position);
        while queue.back().is_some_and(|&back| {
            let old = value_at(back);
            if maximum { value >= old } else { value <= old }
        }) {
            queue.pop_back();
        }
        queue.push_back(position);
        while queue
            .front()
            .is_some_and(|&front| front + radius * 2 < position)
        {
            queue.pop_front();
        }
        if position >= radius * 2 {
            output[base + (position - radius * 2) * stride] =
                value_at(queue.front().copied().unwrap_or(position));
        }
    }
}

fn composite_color(target: &mut [f32; 4], color: [f32; 3], coverage: f32) {
    let coverage = coverage.clamp(0.0, 1.0);
    if coverage <= 0.0 {
        return;
    }
    for channel in 0..3 {
        target[channel] = color[channel] * coverage + target[channel] * (1.0 - coverage);
    }
    target[3] = coverage + target[3] * (1.0 - coverage);
}

fn composite_premultiplied(target: &mut [f32; 4], source: [f32; 4]) {
    let alpha = source[3].clamp(0.0, 1.0);
    for channel in 0..3 {
        target[channel] = source[channel] + target[channel] * (1.0 - alpha);
    }
    target[3] = alpha + target[3] * (1.0 - alpha);
}

pub fn bake_layer_effects(
    pixels: &RgbaImage,
    transform: Transform,
    mask: Option<&Mask>,
    effects: LayerEffects,
) -> Option<(RgbaImage, Transform)> {
    if !effects.renders() || pixels.width() == 0 || pixels.height() == 0 {
        return None;
    }
    let margin = effect_margin(&effects);
    let width = pixels.width().checked_add(margin * 2)?;
    let height = pixels.height().checked_add(margin * 2)?;
    let total = width as usize * height as usize;
    let source_width = pixels.width();
    let source_height = pixels.height();
    let mut alpha = vec![0.0; total];
    let mut source = vec![[0.0; 4]; total];
    for y in 0..source_height {
        for x in 0..source_width {
            let pixel = pixels.get_pixel(x, y).0;
            let mask_value = mask.filter(|mask| mask.enabled).map_or(1.0, |mask| {
                render::mask_sample(
                    &mask.pixels,
                    mask.placement.unwrap_or(transform).inverse(Point::new(
                        (x as f32 + 0.5) / source_width as f32,
                        (y as f32 + 0.5) / source_height as f32,
                    )),
                )
            });
            let coverage = pixel[3] as f32 / 255.0 * mask_value;
            let index = (y + margin) as usize * width as usize + (x + margin) as usize;
            alpha[index] = coverage;
            source[index][3] = coverage;
            for channel in 0..3 {
                source[index][channel] = pixel[channel] as f32 / 255.0 * coverage;
            }
        }
    }
    let mut output = vec![[0.0; 4]; total];
    if let Some(effect) = effects
        .shadow
        .as_ref()
        .filter(|effect| effect.enabled && effect.opacity > 0.0)
    {
        let (sin, cos) = effect.angle.to_radians().sin_cos();
        let shifted = shift_alpha(
            &alpha,
            width as usize,
            height as usize,
            -effect.distance * cos,
            effect.distance * sin,
        );
        let blurred = gaussian_alpha(&shifted, width as usize, height as usize, effect.blur);
        for (target, coverage) in output.iter_mut().zip(blurred) {
            composite_color(target, effect.color, coverage * effect.opacity);
        }
    }
    if let Some(effect) = effects
        .outer_glow
        .as_ref()
        .filter(|effect| effect.enabled && effect.size > 0.0 && effect.opacity > 0.0)
    {
        let blurred = gaussian_alpha(&alpha, width as usize, height as usize, effect.size);
        for (index, (target, &source_alpha)) in output.iter_mut().zip(alpha.iter()).enumerate() {
            composite_color(
                target,
                effect.color,
                blurred[index] * (1.0 - source_alpha) * effect.opacity,
            );
        }
    }
    if let Some(effect) = effects.stroke.as_ref().filter(|effect| {
        effect.enabled && effect.size > 0.0 && effect.opacity > 0.0 && !effect.inside
    }) {
        let dilated = morphology(
            &alpha,
            width as usize,
            height as usize,
            effect.size.round().max(1.0) as usize,
            true,
        );
        for (index, (target, &source_alpha)) in output.iter_mut().zip(alpha.iter()).enumerate() {
            composite_color(
                target,
                effect.color,
                (dilated[index] - source_alpha).max(0.0) * effect.opacity,
            );
        }
    }
    for (target, source_pixel) in output.iter_mut().zip(source) {
        composite_premultiplied(target, source_pixel);
    }
    if let Some(effect) = effects
        .color_overlay
        .as_ref()
        .filter(|effect| effect.enabled && effect.opacity > 0.0)
    {
        for (target, &coverage) in output.iter_mut().zip(alpha.iter()) {
            composite_color(target, effect.color, coverage * effect.opacity);
        }
    }
    if let Some(effect) = effects
        .inner_glow
        .as_ref()
        .filter(|effect| effect.enabled && effect.size > 0.0 && effect.opacity > 0.0)
    {
        let blurred = gaussian_alpha(&alpha, width as usize, height as usize, effect.size);
        for (target, (&coverage, &blurred_alpha)) in
            output.iter_mut().zip(alpha.iter().zip(blurred.iter()))
        {
            composite_color(
                target,
                effect.color,
                coverage * (1.0 - blurred_alpha) * effect.opacity,
            );
        }
    }
    if let Some(effect) = effects
        .inner_shadow
        .as_ref()
        .filter(|effect| effect.enabled && effect.opacity > 0.0)
    {
        let (sin, cos) = effect.angle.to_radians().sin_cos();
        let shifted = shift_alpha(
            &alpha,
            width as usize,
            height as usize,
            -effect.distance * cos,
            effect.distance * sin,
        );
        let blurred = gaussian_alpha(&shifted, width as usize, height as usize, effect.blur);
        for (index, (target, &coverage)) in output.iter_mut().zip(alpha.iter()).enumerate() {
            composite_color(
                target,
                effect.color,
                coverage * (1.0 - blurred[index]) * effect.opacity,
            );
        }
    }
    if let Some(effect) = effects.stroke.as_ref().filter(|effect| {
        effect.enabled && effect.size > 0.0 && effect.opacity > 0.0 && effect.inside
    }) {
        let eroded = morphology(
            &alpha,
            width as usize,
            height as usize,
            effect.size.round().max(1.0) as usize,
            false,
        );
        for (index, (target, &source_alpha)) in output.iter_mut().zip(alpha.iter()).enumerate() {
            composite_color(
                target,
                effect.color,
                (source_alpha - eroded[index]).max(0.0) * effect.opacity,
            );
        }
    }
    let image = RgbaImage::from_fn(width, height, |x, y| {
        let pixel = output[y as usize * width as usize + x as usize];
        let alpha = pixel[3].clamp(0.0, 1.0);
        let color = if alpha > 0.000001 {
            [
                (pixel[0] / alpha).clamp(0.0, 1.0),
                (pixel[1] / alpha).clamp(0.0, 1.0),
                (pixel[2] / alpha).clamp(0.0, 1.0),
            ]
        } else {
            [0.0; 3]
        };
        Rgba([
            (color[0] * 255.0).round() as u8,
            (color[1] * 255.0).round() as u8,
            (color[2] * 255.0).round() as u8,
            (alpha * 255.0).round() as u8,
        ])
    });
    let expanded = transform.expanded(
        -(margin as f32) / source_width as f32,
        -(margin as f32) / source_height as f32,
        1.0 + margin as f32 / source_width as f32,
        1.0 + margin as f32 / source_height as f32,
    );
    Some((image, expanded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{
        ColorOverlayEffect, InnerGlowEffect, InnerShadowEffect, OuterGlowEffect, ShadowEffect,
        StrokeEffect,
    };

    #[test]
    fn morphology_uses_square_dilation_and_zero_padded_erosion() {
        let mut alpha = vec![0.0; 25];
        alpha[12] = 1.0;
        let dilated = morphology(&alpha, 5, 5, 1, true);
        assert!(dilated.iter().enumerate().all(|(index, value)| {
            let inside = (1..=3).contains(&(index % 5)) && (1..=3).contains(&(index / 5));
            *value == if inside { 1.0 } else { 0.0 }
        }));
        let eroded = morphology(&alpha, 5, 5, 1, false);
        assert!(eroded.iter().all(|value| *value == 0.0));
    }

    #[test]
    fn color_overlay_uses_source_coverage_and_keeps_placement() {
        let effects = LayerEffects {
            color_overlay: Some(ColorOverlayEffect {
                color: [1.0, 0.0, 0.0],
                opacity: 0.5,
                ..ColorOverlayEffect::default()
            }),
            ..LayerEffects::default()
        };
        let source = RgbaImage::from_pixel(1, 1, Rgba([255, 255, 255, 255]));
        let mut transform = Transform::new(1, 1);
        transform.x = 12.0;
        transform.y = 9.0;
        let (image, expanded) = bake_layer_effects(&source, transform, None, effects).unwrap();
        let margin = effect_margin(&effects);
        let pixel = image.get_pixel(margin, margin).0;
        assert_eq!(pixel, [255, 128, 128, 255]);
        assert_eq!(
            expanded.point(Point::new(0.5, 0.5)),
            transform.point(Point::new(0.5, 0.5))
        );
    }

    #[test]
    fn outer_effects_remain_behind_the_source() {
        let effects = LayerEffects {
            shadow: Some(ShadowEffect {
                distance: 2.0,
                blur: 1.0,
                ..ShadowEffect::default()
            }),
            outer_glow: Some(OuterGlowEffect {
                size: 2.0,
                ..OuterGlowEffect::default()
            }),
            stroke: Some(StrokeEffect {
                size: 1.0,
                ..StrokeEffect::default()
            }),
            ..LayerEffects::default()
        };
        let source = RgbaImage::from_pixel(3, 3, Rgba([255, 0, 0, 255]));
        let (image, _) = bake_layer_effects(&source, Transform::new(3, 3), None, effects).unwrap();
        let margin = effect_margin(&effects);
        assert!(image.get_pixel(margin, margin + 2)[3] > 0);
        assert_eq!(image.get_pixel(margin + 1, margin + 1).0, [255, 0, 0, 255]);
    }

    #[test]
    fn all_effects_bake_and_disabled_effects_do_not_change_pixels() {
        let effects = LayerEffects {
            stroke: Some(StrokeEffect::default()),
            shadow: Some(ShadowEffect::default()),
            color_overlay: Some(ColorOverlayEffect::default()),
            inner_shadow: Some(InnerShadowEffect::default()),
            outer_glow: Some(OuterGlowEffect::default()),
            inner_glow: Some(InnerGlowEffect::default()),
        };
        let source = RgbaImage::from_pixel(2, 2, Rgba([20, 80, 140, 180]));
        let (image, transform) =
            bake_layer_effects(&source, Transform::new(2, 2), None, effects).unwrap();
        assert!(image.width() > source.width());
        assert!(transform.valid());
        let disabled = LayerEffects::default();
        assert!(!disabled.renders());
        assert!(bake_layer_effects(&source, Transform::new(2, 2), None, disabled).is_none());
    }

    #[test]
    fn vignette_uses_selected_color_and_preserves_alpha() {
        let source = RgbaImage::from_pixel(41, 41, Rgba([128, 128, 128, 128]));
        let result = filtered(
            &source,
            &Filter::Vignette {
                amount: 100.0,
                color: [1.0, 0.0, 0.0],
                midpoint: 50.0,
                roundness: 0.0,
                feather: 60.0,
                highlights: 0.0,
            },
        );
        let center = result.get_pixel(20, 20).0;
        let corner = result.get_pixel(0, 0).0;
        assert_eq!(center[3], 128);
        assert!(center[0].abs_diff(center[1]) <= 2);
        assert!(corner[0] > corner[1] + 50);
    }

    #[test]
    fn vignette_fills_an_empty_layer_and_keeps_effects() {
        let mut document = Document::new(24, 24).unwrap();
        let effects = LayerEffects {
            color_overlay: Some(ColorOverlayEffect::default()),
            ..LayerEffects::default()
        };
        document.active_mut().unwrap().effects = Some(effects);
        apply_filter(
            &mut document,
            &Filter::Vignette {
                amount: 100.0,
                color: [1.0, 0.0, 0.0],
                midpoint: 50.0,
                roundness: 100.0,
                feather: 60.0,
                highlights: 0.0,
            },
            false,
        )
        .unwrap();
        let layer = document.active().unwrap();
        let pixels = layer.pixels.as_ref().unwrap();
        assert!(pixels.get_pixel(0, 0)[3] > 0);
        assert_eq!(pixels.get_pixel(12, 12)[3], 0);
        assert_eq!(layer.effects, Some(effects));
    }

    #[test]
    fn bloom_spreads_highlights_through_transparency() {
        let mut source = RgbaImage::from_pixel(65, 65, Rgba([0, 0, 0, 255]));
        for y in 30..35 {
            for x in 30..35 {
                source.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
        let result = filtered(
            &source,
            &Filter::BloomGlow {
                amount: 100.0,
                radius: 12.0,
            },
        );
        assert!(result.get_pixel(40, 32)[0] > result.get_pixel(2, 2)[0]);
        assert!(result.get_pixel(40, 32)[0] > 0);

        let mut transparent = RgbaImage::new(65, 65);
        for y in 30..35 {
            for x in 30..35 {
                transparent.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
        let spread = filtered(
            &transparent,
            &Filter::BloomGlow {
                amount: 100.0,
                radius: 12.0,
            },
        );
        assert!(spread.get_pixel(40, 32)[3] > 0);
    }

    #[test]
    fn tonal_contrast_expands_midtone_detail_without_changing_alpha() {
        let source = RgbaImage::from_fn(64, 16, |x, _| {
            let value = if (x / 8) % 2 == 0 { 102 } else { 153 };
            Rgba([value, value, value, 255])
        });
        let result = filtered(
            &source,
            &Filter::TonalContrast {
                amount: 100.0,
                radius: 6.0,
                shadows: 0.0,
                midtones: 100.0,
                highlights: 0.0,
            },
        );
        assert!(result.get_pixel(5, 8)[0] < source.get_pixel(5, 8)[0]);
        assert!(result.get_pixel(13, 8)[0] > source.get_pixel(13, 8)[0]);
        assert_eq!(result.get_pixel(5, 8)[3], 255);
    }

    #[test]
    fn cancelled_finishing_filter_leaves_document_untouched() {
        let mut document = Document::new(16, 16).unwrap();
        document.active_mut().unwrap().pixels = Some(Arc::new(RgbaImage::from_pixel(
            16,
            16,
            Rgba([120, 80, 40, 255]),
        )));
        let original = document.clone();
        let cancel = AtomicBool::new(true);
        assert!(
            apply_filter_cancellable(
                &mut document,
                &Filter::TonalContrast {
                    amount: 100.0,
                    radius: 6.0,
                    shadows: 0.0,
                    midtones: 100.0,
                    highlights: 0.0,
                },
                false,
                &cancel,
            )
            .is_err()
        );
        assert_eq!(
            document.active().unwrap().pixels,
            original.active().unwrap().pixels
        );
        assert_eq!(
            document.active().unwrap().transform,
            original.active().unwrap().transform
        );
    }

    // The original implementation is an independent reference for sampling and alpha.
    fn reference_motion_blur(image: &RgbaImage, distance: f32, angle: f32) -> RgbaImage {
        let (w, h) = image.dimensions();
        let steps = distance.ceil().clamp(1.0, 256.0) as u32;
        let (sin, cos) = angle.to_radians().sin_cos();
        RgbaImage::from_fn(w, h, |x, y| {
            let mut sum = [0.0; 4];
            for i in 0..steps {
                let offset = ((i as f32 + 0.5) / steps as f32 - 0.5) * distance;
                let p = render::sample(
                    image,
                    Point::new(
                        (x as f32 + 0.5 + offset * cos) / w as f32,
                        (y as f32 + 0.5 + offset * sin) / h as f32,
                    ),
                );
                for c in 0..3 {
                    sum[c] += p[c] * p[3];
                }
                sum[3] += p[3];
            }
            if sum[3] > 0.0 {
                for c in 0..3 {
                    sum[c] /= sum[3];
                }
            }
            sum[3] /= steps as f32;
            Rgba(sum.map(|v| (v * 255.0).round() as u8))
        })
    }

    #[test]
    fn motion_blur_matches_reference_at_edges_and_arbitrary_angles() {
        for (width, height) in [(31, 19), (1, 7), (7, 1), (0, 0)] {
            let image = RgbaImage::from_fn(width, height, |x, y| {
                Rgba([
                    (x * 67 + y * 41) as u8,
                    (x * 23 + y * 59) as u8,
                    (x * 13 + y * 17) as u8,
                    if (x + y) % 3 == 0 {
                        0
                    } else {
                        (x * 53 + y * 97) as u8
                    },
                ])
            });
            for distance in [1.0, 4.0, 20.0, 23.7, 200.0, 300.0] {
                for angle in [0.0, 90.0, -90.0, 180.0, 35.0, -35.0] {
                    let expected = reference_motion_blur(&image, distance, angle);
                    let actual = filtered(&image, &Filter::MotionBlur { distance, angle });
                    for (p, q) in actual.pixels().zip(expected.pixels()) {
                        for c in 0..4 {
                            // RGB is undefined when both results are fully transparent.
                            if c < 3 && p[3] == 0 && q[3] == 0 {
                                continue;
                            }
                            assert!(
                                p[c].abs_diff(q[c]) <= 1,
                                "{width}x{height}, distance {distance}, angle {angle}: {p:?} != {q:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn motion_blur_preserves_color_selection_and_mask_placement() {
        let mut doc = Document::new(20, 20).unwrap();
        let mut layer = crate::document::Layer::image(
            "red",
            RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255])),
        );
        layer.transform.x = 8.0;
        layer.transform.y = 8.0;
        layer.mask = Some(crate::document::Mask::white());
        let transform = layer.transform;
        doc.insert(layer);
        doc.selection = Some(Arc::new(image::GrayImage::from_fn(20, 20, |_, y| {
            image::Luma([if y >= 10 { 255 } else { 0 }])
        })));
        apply_filter(
            &mut doc,
            &Filter::MotionBlur {
                distance: 4.0,
                angle: 0.0,
            },
            false,
        )
        .unwrap();
        let layer = doc.active().unwrap();
        assert_eq!(layer.mask.as_ref().unwrap().placement, Some(transform));
        let pixels = layer.pixels.as_ref().unwrap();
        assert_eq!(pixels.dimensions(), (10, 10));
        assert_eq!(pixels.get_pixel(2, 4).0, [0; 4]);
        assert_eq!(pixels.get_pixel(3, 4).0, [255, 0, 0, 255]);
        assert!(pixels.get_pixel(2, 5)[3] > 0);
        assert_eq!(pixels.get_pixel(2, 5)[0], 255);
        doc.validate().unwrap();
    }

    #[test]
    fn cancelled_motion_blur_leaves_document_untouched() {
        let mut doc = Document::new(20, 20).unwrap();
        let original = doc.clone();
        assert!(
            apply_filter_cancellable(
                &mut doc,
                &Filter::MotionBlur {
                    distance: 200.0,
                    angle: 35.0
                },
                false,
                &AtomicBool::new(true),
            )
            .is_err()
        );
        assert_eq!(
            doc.active().unwrap().pixels,
            original.active().unwrap().pixels
        );
        assert_eq!(
            doc.active().unwrap().transform,
            original.active().unwrap().transform
        );
    }

    #[test]
    #[ignore = "manual performance comparison with the original motion blur"]
    fn motion_blur_benchmark() {
        let image = RgbaImage::from_fn(1024, 768, |x, y| {
            Rgba([x as u8, y as u8, (x + y) as u8, 255])
        });
        for (distance, angle) in [(20.0, 0.0), (200.0, 35.0)] {
            let start = std::time::Instant::now();
            let expected = reference_motion_blur(&image, distance, angle);
            let original = start.elapsed();
            let start = std::time::Instant::now();
            let actual = filtered(&image, &Filter::MotionBlur { distance, angle });
            let optimized = start.elapsed();
            assert!(
                actual
                    .as_raw()
                    .iter()
                    .zip(expected.as_raw())
                    .all(|(a, b)| a.abs_diff(*b) <= 1)
            );
            eprintln!(
                "1024x768, distance {distance}, angle {angle}: original {original:?}, optimized {optimized:?} ({:.1}x)",
                original.as_secs_f64() / optimized.as_secs_f64()
            );
        }
    }

    #[test]
    fn blur_spreads_beyond_bounds_and_preserves_color() {
        let mut doc = Document::new(20, 20).unwrap();
        let mut layer = crate::document::Layer::image(
            "red",
            RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255])),
        );
        layer.transform.x = 8.0;
        layer.transform.y = 8.0;
        doc.insert(layer);
        apply_filter(&mut doc, &Filter::GaussianBlur { radius: 1.0 }, false).unwrap();
        let image = render::render(&doc);
        assert!(image.get_pixel(7, 9)[3] > 0);
        assert_eq!(image.get_pixel(7, 9)[0], 255);
        assert!(image.get_pixel(9, 9)[3] < 255);
        doc.validate().unwrap();
    }

    #[test]
    fn saturation_keeps_grays_neutral_and_exposure_uses_linear_light() {
        let saturated = adjust(
            [0.5, 0.5, 0.5, 1.0],
            &Adjustment::HueSaturation {
                hue: 60.0,
                saturation: 100.0,
                lightness: 0.0,
                colorize: false,
            },
            Point::default(),
        );
        assert_eq!(saturated, [0.5, 0.5, 0.5, 1.0]);
        let exposed = adjust(
            [0.5, 0.5, 0.5, 1.0],
            &Adjustment::Exposure {
                exposure: 1.0,
                offset: 0.0,
                gamma: 1.0,
            },
            Point::default(),
        );
        assert!((exposed[0] - 0.685_836).abs() < 0.00001);
    }

    #[test]
    fn identity_adjustments_and_hue_rotation() {
        let p = [0.2, 0.5, 0.8, 0.7];
        let a = Adjustment::HueSaturation {
            hue: 0.0,
            saturation: 0.0,
            lightness: 0.0,
            colorize: false,
        };
        let q = adjust(p, &a, Point::default());
        for i in 0..4 {
            assert!((p[i] - q[i]).abs() < 0.00001);
        }
        let green = adjust(
            [1.0, 0.0, 0.0, 1.0],
            &Adjustment::HueSaturation {
                hue: 120.0,
                saturation: 0.0,
                lightness: 0.0,
                colorize: false,
            },
            Point::default(),
        );
        assert!(green[0] < 0.001 && green[1] > 0.999 && green[2] < 0.001);
    }

    /// Photoshop's Black & White defaults, with the tint set for a test.
    fn black_white(tint: bool, tint_hue: f32, tint_saturation: f32) -> Adjustment {
        Adjustment::BlackWhite {
            reds: 40.0,
            yellows: 60.0,
            greens: 40.0,
            cyans: 60.0,
            blues: 20.0,
            magentas: 80.0,
            tint,
            tint_hue,
            tint_saturation,
        }
    }

    fn color_balance(
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
        preserve_luminosity: bool,
    ) -> Adjustment {
        Adjustment::ColorBalance {
            shadows,
            midtones,
            highlights,
            preserve_luminosity,
        }
    }

    #[test]
    fn black_and_white_weighs_each_color_family() {
        let gray = |rgb: [f32; 3]| {
            adjust(
                [rgb[0], rgb[1], rgb[2], 1.0],
                &Adjustment::photoshop_black_white(),
                Point::default(),
            )[0]
        };
        // Photoshop's defaults: reds 40, yellows 60, greens 40, cyans 60, blues 20, magentas 80.
        assert!((gray([1.0, 0.0, 0.0]) - 0.4).abs() < 1e-5);
        assert!((gray([1.0, 1.0, 0.0]) - 0.6).abs() < 1e-5);
        assert!((gray([0.0, 1.0, 1.0]) - 0.6).abs() < 1e-5);
        assert!((gray([0.0, 0.0, 1.0]) - 0.2).abs() < 1e-5);
        // Not a desaturation: a plain luminance would put red at 0.299, not 0.4.
        assert!((gray([1.0, 0.0, 0.0]) - 1.0 * 0.299).abs() > 0.05);
    }

    #[test]
    fn black_and_white_tint_colors_the_tones() {
        let tinted = black_white(true, 40.0, 20.0);
        let result = adjust([0.5, 0.5, 0.5, 1.0], &tinted, Point::default());
        assert!(result[0] > result[1] && result[1] > result[2]);
        // The gray tone is kept as the lightness, so a mid gray stays mid.
        assert!((result[0] - 0.6).abs() < 1e-4 && (result[2] - 0.4).abs() < 1e-4);
        // Without the flag the tint is ignored even when its hue is set.
        let plain = black_white(false, 40.0, 20.0);
        let result = adjust([0.5, 0.5, 0.5, 1.0], &plain, Point::default());
        assert_eq!(result, [0.5, 0.5, 0.5, 1.0]);
    }

    #[test]
    fn color_balance_separates_shadows_from_highlights() {
        let neutral = Adjustment::neutral_color_balance();
        let pixel = [0.3, 0.5, 0.7, 1.0];
        assert_eq!(adjust(pixel, &neutral, Point::default()), pixel);
        let warm_shadows = Adjustment::ColorBalance {
            shadows: [100.0, 0.0, 0.0],
            midtones: [0.0; 3],
            highlights: [0.0; 3],
            preserve_luminosity: false,
        };
        let dark = adjust([0.1, 0.1, 0.1, 1.0], &warm_shadows, Point::default());
        let bright = adjust([0.9, 0.9, 0.9, 1.0], &warm_shadows, Point::default());
        assert!(dark[0] - 0.1 > 0.5, "shadows take most of the shift");
        assert!(bright[0] - 0.9 < 0.02, "highlights barely move");
        let warm_highlights = Adjustment::ColorBalance {
            shadows: [0.0; 3],
            midtones: [0.0; 3],
            highlights: [100.0, 0.0, 0.0],
            preserve_luminosity: false,
        };
        let bright = adjust([0.9, 0.9, 0.9, 1.0], &warm_highlights, Point::default());
        assert!(bright[0] - 0.9 > 0.05);
    }

    #[test]
    fn color_balance_can_keep_luminosity() {
        let keeping = Adjustment::ColorBalance {
            shadows: [0.0; 3],
            midtones: [60.0, -40.0, 0.0],
            highlights: [0.0; 3],
            preserve_luminosity: true,
        };
        let letting = color_balance([0.0; 3], [60.0, -40.0, 0.0], [0.0; 3], false);
        let pixel = [0.5, 0.45, 0.55, 1.0];
        let luma = |c: [f32; 4]| c[0] * 0.299 + c[1] * 0.587 + c[2] * 0.114;
        let kept = adjust(pixel, &keeping, Point::default());
        let shifted = adjust(pixel, &letting, Point::default());
        assert!((luma(kept) - luma(pixel)).abs() < 1e-4);
        assert!(kept[0] > pixel[0], "the shift still shows");
        assert!(
            (luma(shifted) - luma(pixel)).abs() > 0.01,
            "without it the shift moves luma"
        );
    }

    #[test]
    fn new_adjustments_validate_their_source_ranges() {
        assert!(validate_adjustment(&Adjustment::photoshop_black_white()).is_ok());
        assert!(validate_adjustment(&Adjustment::neutral_color_balance()).is_ok());
        let mut weights = black_white(false, 40.0, 20.0);
        let Adjustment::BlackWhite { reds, .. } = &mut weights else {
            unreachable!()
        };
        *reds = 400.0;
        assert!(validate_adjustment(&weights).is_err());
        assert!(
            validate_adjustment(&Adjustment::ColorBalance {
                shadows: [101.0, 0.0, 0.0],
                midtones: [0.0; 3],
                highlights: [0.0; 3],
                preserve_luminosity: true,
            })
            .is_err()
        );
    }

    /// Compositor 1.2.3's Add Noise, which is its own kernel rather than the Grain adjustment: a
    /// hashed pattern per pixel, flat or bell shaped, and one seed across the channels or one each.
    #[test]
    fn add_noise_follows_its_seed_and_channel_settings() {
        let point = Point::new(11.0, 7.0);
        let noise = |gaussian, monochromatic, seed| Adjustment::AddNoise {
            amount: 60.0,
            gaussian,
            monochromatic,
            seed,
        };
        let gray = adjust([0.5, 0.5, 0.5, 1.0], &noise(false, true, 7), point);
        let colored = adjust([0.5, 0.5, 0.5, 1.0], &noise(false, false, 7), point);
        assert_eq!(gray[0], gray[1]);
        assert_eq!(gray[1], gray[2]);
        assert!(colored[0] != colored[1] || colored[1] != colored[2]);
        // The same seed and pixel draw the same pattern; another seed draws a different one, and a
        // neighbouring pixel is unrelated to this one.
        assert_eq!(
            gray,
            adjust([0.5, 0.5, 0.5, 1.0], &noise(false, true, 7), point)
        );
        assert_ne!(
            gray,
            adjust([0.5, 0.5, 0.5, 1.0], &noise(false, true, 8), point)
        );
        assert_ne!(
            gray,
            adjust(
                [0.5, 0.5, 0.5, 1.0],
                &noise(false, true, 7),
                Point::new(12.0, 7.0)
            )
        );
        // A flat spread never pushes a pixel further than the amount, while the bell curve does, which
        // is what makes the two settings look different.
        let deviations = |gaussian| {
            (0..400)
                .map(|x| {
                    let point = Point::new(x as f32, 0.0);
                    (adjust([0.5; 4], &noise(gaussian, true, 3), point)[0] - 0.5).abs()
                })
                .collect::<Vec<f32>>()
        };
        let spread = 60.0 / 100.0 * 127.5 / 255.0;
        let flat = deviations(false);
        let bell = deviations(true);
        assert!(flat.iter().all(|deviation| *deviation <= spread + 1e-5));
        assert!(bell.iter().any(|deviation| *deviation > spread));
    }

    #[test]
    fn blur_and_noise_adjustments_validate_their_source_ranges() {
        for valid in [
            Adjustment::GaussianBlur { radius: 0.1 },
            Adjustment::GaussianBlur { radius: 250.0 },
            Adjustment::MotionBlur {
                angle: -90.0,
                distance: 1.0,
            },
            Adjustment::MotionBlur {
                angle: 90.0,
                distance: 2000.0,
            },
            Adjustment::AddNoise {
                amount: 400.0,
                gaussian: true,
                monochromatic: true,
                seed: 0,
            },
        ] {
            assert!(validate_adjustment(&valid).is_ok(), "{valid:?}");
        }
        for invalid in [
            Adjustment::GaussianBlur { radius: 0.0 },
            Adjustment::GaussianBlur { radius: f32::NAN },
            Adjustment::MotionBlur {
                angle: 91.0,
                distance: 10.0,
            },
            Adjustment::MotionBlur {
                angle: 0.0,
                distance: 0.5,
            },
            Adjustment::AddNoise {
                amount: 0.0,
                gaussian: false,
                monochromatic: false,
                seed: 1,
            },
        ] {
            assert!(validate_adjustment(&invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn monotone_curves_do_not_overshoot() {
        let points = [
            Point::new(0.0, 0.0),
            Point::new(0.25, 0.7),
            Point::new(0.75, 0.7),
            Point::new(1.0, 1.0),
        ];
        for i in 25..75 {
            assert!((curve_value(&points, i as f32 / 100.0) - 0.7).abs() < 0.00001);
        }
    }
}
