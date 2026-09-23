//! The blur adjustment layers Compositor 1.2.3 added draw everything beneath them again, softened,
//! rather than changing the pixel they land on. Compositor hands the whole backdrop to Core Image;
//! mectov blurs it here on the CPU and in `composite.wgsl` on the GPU, with the same taps so the two
//! renderers can be held against each other.
//!
//! Compositor draws these at the scale the canvas is being drawn, so a radius or a distance counts
//! document pixels and a preview blurs proportionally less.

use rayon::prelude::*;

use crate::document::Adjustment;

/// Gaussian taps per axis. `composite.wgsl` reads the same count.
pub const GAUSSIAN_TAPS: u32 = 32;

/// One axis of a separable Gaussian: where each tap sits and what it weighs, spread over ±radius with
/// σ = radius / 2 so the outermost taps still carry about an eighth of the centre's weight.
struct Kernel {
    taps: Vec<(f32, f32)>,
    total: f32,
}

impl Kernel {
    fn gaussian(radius: f32) -> Self {
        let sigma = (radius * 0.5).max(0.0001);
        let taps: Vec<(f32, f32)> = (0..GAUSSIAN_TAPS)
            .map(|i| {
                let t = (i as f32 + 0.5) / GAUSSIAN_TAPS as f32 * 2.0 - 1.0;
                let offset = t * radius;
                (offset, (-(offset * offset) / (2.0 * sigma * sigma)).exp())
            })
            .collect();
        let total = taps.iter().map(|(_, weight)| weight).sum();
        Self { taps, total }
    }
}

/// Compositor's filter samples a motion blur once per pixel of distance, capped at 256.
pub fn motion_steps(distance: f32) -> u32 {
    distance.ceil().clamp(1.0, 256.0) as u32
}

/// How far a blur reads outside the rectangle being drawn, in render pixels. A point query renders a
/// rectangle this much larger and takes its centre, so the kernel never runs off the edge.
pub fn margin(adjustment: &Adjustment, scale: f32) -> f32 {
    match adjustment {
        Adjustment::GaussianBlur { radius } => radius * scale,
        Adjustment::MotionBlur { distance, .. } => distance * scale * 0.5,
        _ => 0.0,
    }
}

fn scale4(pixel: [f32; 4], factor: f32) -> [f32; 4] {
    [
        pixel[0] * factor,
        pixel[1] * factor,
        pixel[2] * factor,
        pixel[3] * factor,
    ]
}

fn add(left: [f32; 4], right: [f32; 4]) -> [f32; 4] {
    [
        left[0] + right[0],
        left[1] + right[1],
        left[2] + right[2],
        left[3] + right[3],
    ]
}

/// A buffer a blur reads from, in straight color or already premultiplied. Coordinates count texels,
/// so an integer lands on a pixel centre, and anything outside the buffer is transparent.
struct Surface<'a> {
    pixels: &'a [[f32; 4]],
    width: usize,
    height: usize,
    premultiplied: bool,
}

impl Surface<'_> {
    fn pixel(&self, x: i64, y: i64) -> [f32; 4] {
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
            return [0.0; 4];
        }
        let pixel = self.pixels[y as usize * self.width + x as usize];
        if self.premultiplied {
            return pixel;
        }
        [
            pixel[0] * pixel[3],
            pixel[1] * pixel[3],
            pixel[2] * pixel[3],
            pixel[3],
        ]
    }

    fn sample(&self, x: f32, y: f32) -> [f32; 4] {
        let (lx, ly) = (x.floor(), y.floor());
        let (fx, fy) = (x - lx, y - ly);
        let mut sum = [0.0; 4];
        for (dx, wx) in [(0.0, 1.0 - fx), (1.0, fx)] {
            for (dy, wy) in [(0.0, 1.0 - fy), (1.0, fy)] {
                let weight = wx * wy;
                if weight == 0.0 {
                    continue;
                }
                sum = add(
                    sum,
                    scale4(
                        self.pixel(lx as i64 + dx as i64, ly as i64 + dy as i64),
                        weight,
                    ),
                );
            }
        }
        sum
    }
}

fn along(surface: &Surface, kernel: &Kernel, x: f32, y: f32, dx: f32, dy: f32) -> [f32; 4] {
    let mut sum = [0.0; 4];
    for (offset, weight) in &kernel.taps {
        sum = add(
            sum,
            scale4(surface.sample(x + offset * dx, y + offset * dy), *weight),
        );
    }
    scale4(sum, 1.0 / kernel.total.max(0.000001))
}

/// Lay a blurred pixel over the one it replaces. Both sides are premultiplied while they are mixed, so
/// the color of a transparent pixel cannot leak into its neighbours, and the layer's own opacity thins
/// the softened result the way it thins anything else the layer draws.
fn combine(original: [f32; 4], blurred: [f32; 4], amount: f32) -> [f32; 4] {
    let kept = amount.clamp(0.0, 1.0);
    let mixed = [
        original[0] * original[3] * (1.0 - kept) + blurred[0] * kept,
        original[1] * original[3] * (1.0 - kept) + blurred[1] * kept,
        original[2] * original[3] * (1.0 - kept) + blurred[2] * kept,
        original[3] * (1.0 - kept) + blurred[3] * kept,
    ];
    if mixed[3] <= 0.000001 {
        return [0.0, 0.0, 0.0, mixed[3]];
    }
    [
        mixed[0] / mixed[3],
        mixed[1] / mixed[3],
        mixed[2] / mixed[3],
        mixed[3],
    ]
}

/// Blur what has been drawn so far, in place, where `amounts` decides how much of the softened
/// backdrop replaces the sharp one. `scale` is how many render pixels one document pixel covers.
pub fn blur(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    adjustment: &Adjustment,
    amounts: &[f32],
    scale: f32,
) {
    if width == 0 || height == 0 || pixels.len() < width * height {
        return;
    }
    match adjustment {
        Adjustment::GaussianBlur { radius } => {
            // Separable, exactly as the shader runs it: one axis into a scratch buffer, then the
            // other, which redraws the backdrop softened where the layer applies.
            let kernel = Kernel::gaussian(radius * scale);
            let mut horizontal = vec![[0.0_f32; 4]; pixels.len()];
            let straight = Surface {
                pixels,
                width,
                height,
                premultiplied: false,
            };
            horizontal
                .par_chunks_mut(width)
                .enumerate()
                .for_each(|(y, row)| {
                    for (x, target) in row.iter_mut().enumerate() {
                        *target = along(&straight, &kernel, x as f32, y as f32, 1.0, 0.0);
                    }
                });
            let softened = Surface {
                pixels: &horizontal,
                width,
                height,
                premultiplied: true,
            };
            pixels
                .par_chunks_mut(width)
                .enumerate()
                .for_each(|(y, row)| {
                    for (x, target) in row.iter_mut().enumerate() {
                        let blurred = along(&softened, &kernel, x as f32, y as f32, 0.0, 1.0);
                        *target = combine(*target, blurred, amounts[y * width + x]);
                    }
                });
        }
        Adjustment::MotionBlur { angle, distance } => {
            let distance = distance * scale;
            let steps = motion_steps(distance);
            let (sin, cos) = angle.to_radians().sin_cos();
            let (dx, dy) = (cos * distance, sin * distance);
            // Every output pixel reads the whole line, so the sharp backdrop is kept aside first.
            let sharp = pixels.to_vec();
            let straight = Surface {
                pixels: &sharp,
                width,
                height,
                premultiplied: false,
            };
            pixels
                .par_chunks_mut(width)
                .enumerate()
                .for_each(|(y, row)| {
                    for (x, target) in row.iter_mut().enumerate() {
                        let mut sum = [0.0_f32; 4];
                        for i in 0..steps {
                            let offset = (i as f32 + 0.5) / steps as f32 - 0.5;
                            sum = add(
                                sum,
                                straight.sample(x as f32 + offset * dx, y as f32 + offset * dy),
                            );
                        }
                        let blurred = scale4(sum, 1.0 / steps as f32);
                        *target = combine(*target, blurred, amounts[y * width + x]);
                    }
                });
        }
        _ => {}
    }
}
