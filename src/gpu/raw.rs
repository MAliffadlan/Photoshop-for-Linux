use super::{Processor, processor::attempt};
use crate::raw::{DecodedRaw, DevelopSettings, GlowStyle, Overlay, OverlayKind};
use anyhow::{Result, ensure};
use std::sync::atomic::{AtomicBool, Ordering};

const SHADER: &str = concat!(
    include_str!("buffers.wgsl"),
    include_str!("raster.wgsl"),
    include_str!("raw.wgsl")
);

pub(crate) fn develop(
    raw: &DecodedRaw,
    s: &DevelopSettings,
    wb: [f32; 3],
    depth: u32,
    cancel: &AtomicBool,
) -> Option<Vec<u8>> {
    attempt(
        u64::from(raw.camera.width()) * u64::from(raw.camera.height()),
        16_384,
        |gpu| gpu.develop(raw, s, wb, depth, cancel),
    )
}

impl Processor {
    pub(super) fn develop(
        &self,
        raw: &DecodedRaw,
        s: &DevelopSettings,
        wb: [f32; 3],
        depth: u32,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>> {
        s.validate()?;
        let size = [raw.camera.width(), raw.camera.height()];
        let count = u64::from(size[0]) * u64::from(size[1]);
        let source = self.buffer(bytemuck::cast_slice(raw.camera.as_raw()))?;
        let buffers = [self.empty(count * 16)?, self.empty(count * 16)?];
        let scale = size[0] as f32 / raw.metadata.width as f32;
        let mut config = settings(raw, s, wb, depth, scale);
        let mut encoder = self.encoder();
        ensure!(!cancel.load(Ordering::Relaxed), "RAW development cancelled");
        self.dispatch(
            &mut encoder,
            "raw_camera",
            SHADER,
            [&source, &source, &buffers[0]],
            &config,
            size,
        )?;
        let mut current = 0;
        if s.luminance_noise > 0.0 || s.color_noise > 0.0 {
            self.dispatch(
                &mut encoder,
                "raw_denoise",
                SHADER,
                [&buffers[current], &source, &buffers[1 - current]],
                &config,
                size,
            )?;
            current = 1 - current;
        }
        for overlay in s.overlays.iter().filter(|o| o.enabled) {
            ensure!(!cancel.load(Ordering::Relaxed), "RAW development cancelled");
            let overlay = overlay_config(overlay, size, cancel)?;
            self.dispatch(
                &mut encoder,
                "raw_overlay",
                SHADER,
                [&buffers[current], &source, &buffers[1 - current]],
                &overlay,
                size,
            )?;
            current = 1 - current;
        }
        self.dispatch(
            &mut encoder,
            "raw_tone",
            SHADER,
            [&buffers[current], &source, &buffers[1 - current]],
            &config,
            size,
        )?;
        current = 1 - current;
        // Compositor's glow source is the luma of the picture before texture and
        // clarity have their say, so it is taken here; the glow itself lands
        // after them, in the effects pass below.
        let glow = if s.glow > 0.0 {
            let plane = self.empty(count * 16)?;
            self.dispatch(
                &mut encoder,
                "raw_glow_source",
                SHADER,
                [&buffers[current], &buffers[current], &plane],
                &config,
                size,
            )?;
            let blurred = self.empty(count * 16)?;
            let radius = config[37][1];
            for (source, target, direction) in [
                (&plane, &blurred, [1.0_f32, 0.0]),
                (&blurred, &plane, [0.0_f32, 1.0]),
            ] {
                self.dispatch(
                    &mut encoder,
                    "raw_glow_blur",
                    SHADER,
                    [source, source, target],
                    &[config[0], [radius, 0.0, direction[0], direction[1]]],
                    size,
                )?;
            }
            // The vertical pass ends back in the plane the compose pass reads.
            Some(plane)
        } else {
            None
        };
        if s.clarity != 0.0 || s.texture != 0.0 || s.sharpen != 0.0 {
            let scratch = self.empty(count * 16)?;
            let blurred = self.empty(count * 16)?;
            for (amount, radius, threshold) in [
                (s.clarity / 100.0, 24.0 * scale, 0.0),
                (s.texture / 100.0, 3.0 * scale, 0.0),
            ] {
                if amount == 0.0 {
                    continue;
                }
                ensure!(!cancel.load(Ordering::Relaxed), "RAW development cancelled");
                self.blur_passes(
                    &mut encoder,
                    &buffers[current],
                    &scratch,
                    &blurred,
                    size,
                    radius.max(0.3),
                )?;
                self.dispatch(
                    &mut encoder,
                    "raw_detail",
                    SHADER,
                    [&buffers[current], &blurred, &buffers[1 - current]],
                    &[config[0], [amount, threshold, 0.0, 0.0]],
                    size,
                )?;
                current = 1 - current;
            }
        }
        // Glow, then the post-crop vignette, then grain: Compositor's order,
        // after the creative grade and before the sharpening that closes the
        // chain.
        if s.adjusts_effects() {
            ensure!(!cancel.load(Ordering::Relaxed), "RAW development cancelled");
            let plane = glow.unwrap_or_else(|| self.empty(count * 16).expect("buffer"));
            self.dispatch(
                &mut encoder,
                "raw_effects",
                SHADER,
                [&buffers[current], &plane, &buffers[1 - current]],
                &config,
                size,
            )?;
            current = 1 - current;
        }
        if s.sharpen != 0.0 {
            let scratch = self.empty(count * 16)?;
            let blurred = self.empty(count * 16)?;
            ensure!(!cancel.load(Ordering::Relaxed), "RAW development cancelled");
            self.blur_passes(
                &mut encoder,
                &buffers[current],
                &scratch,
                &blurred,
                size,
                (s.sharpen_radius * scale).max(0.3),
            )?;
            self.dispatch(
                &mut encoder,
                "raw_detail",
                SHADER,
                [&buffers[current], &blurred, &buffers[1 - current]],
                &[
                    config[0],
                    [s.sharpen / 100.0, s.sharpen_threshold, 0.0, 0.0],
                ],
                size,
            )?;
            current = 1 - current;
        }
        let [left, top, right, bottom] = crop(s, size);
        let target = [right - left, bottom - top];
        config[13] = [left as f32, top as f32, target[0] as f32, target[1] as f32];
        let bytes = u64::from(target[0]) * u64::from(target[1]) * u64::from(depth / 2);
        let output = self.empty(bytes)?;
        ensure!(!cancel.load(Ordering::Relaxed), "RAW development cancelled");
        self.dispatch(
            &mut encoder,
            "raw_encode",
            SHADER,
            [&buffers[current], &source, &output],
            &config,
            target,
        )?;
        let result = self.read(encoder, &output, bytes)?;
        ensure!(!cancel.load(Ordering::Relaxed), "RAW development cancelled");
        Ok(result)
    }
}

pub(crate) fn crop(s: &DevelopSettings, size: [u32; 2]) -> [u32; 4] {
    [
        (s.crop[0] * size[0] as f32).floor() as u32,
        (s.crop[1] * size[1] as f32).floor() as u32,
        (s.crop[2] * size[0] as f32).ceil().min(size[0] as f32) as u32,
        (s.crop[3] * size[1] as f32).ceil().min(size[1] as f32) as u32,
    ]
}

fn settings(
    raw: &DecodedRaw,
    s: &DevelopSettings,
    wb: [f32; 3],
    depth: u32,
    scale: f32,
) -> Vec<[f32; 4]> {
    let (sin, cos) = s.rotation.to_radians().sin_cos();
    let mut p = vec![[0.0; 4]; 43];
    p[0] = [
        raw.camera.width() as f32,
        raw.camera.height() as f32,
        0.0,
        0.0,
    ];
    p[1] = [cos, sin, s.perspective[0], s.perspective[1]];
    p[2] = [s.distortion, s.chromatic_red, s.chromatic_blue, s.vignette];
    p[3] = [wb[0], wb[1], wb[2], 2.0_f32.powf(s.exposure)];
    for (i, row) in raw.camera_to_rgb.iter().enumerate() {
        p[4 + i] = [row[0], row[1], row[2], 0.0];
    }
    p[7] = [s.luminance_noise, s.color_noise, 0.0, 0.0];
    p[8] = [s.shadows, s.highlights, s.whites, s.blacks];
    p[9] = [s.dehaze, s.brightness, s.contrast, s.defringe];
    p[10] = [
        s.saturation,
        s.vibrance,
        if s.monochrome { 1.0 } else { 0.0 },
        s.tone_balance,
    ];
    p[11] = [s.bw_mix[0], s.bw_mix[1], s.bw_mix[2], 0.0];
    p[12] = [
        s.shadow_tone[0],
        s.shadow_tone[1],
        s.highlight_tone[0],
        s.highlight_tone[1],
    ];
    p[15] = [
        s.grading.blending,
        s.grading.balance,
        if s.grading.adjusts() { 1.0 } else { 0.0 },
        0.0,
    ];
    p[14][0] = depth as f32;
    for (i, curve) in s.curves.iter().enumerate() {
        p[16 + i * 2].copy_from_slice(&curve[..4]);
        p[17 + i * 2][0] = curve[4];
    }
    for (i, hue) in [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 270.0, 300.0]
        .iter()
        .enumerate()
    {
        p[24 + i] = [s.hsl[i][0], s.hsl[i][1], s.hsl[i][2], *hue];
    }
    for (i, wheel) in s.grading.wheels().into_iter().enumerate() {
        p[32 + i] = [
            wheel.hue,
            wheel.saturation,
            wheel.luminance,
            if wheel.adjusts() { 1.0 } else { 0.0 },
        ];
    }
    effects(s, scale, &mut p);
    p
}

/// The post-grade effects and the calibration, packed for the shader. Compositor
/// measures both glow's radius and grain's size in document pixels, so the
/// preview's own scale is folded in here rather than in the shader: a preview
/// that is a third of the size asks for a third of the radius and three times
/// the grain scale.
fn effects(s: &DevelopSettings, scale: f32, p: &mut [[f32; 4]]) {
    // Bloom is the tighter of the two soft looks, so it starts from a smaller
    // radius; Halation spreads Diffusion's.
    let base = if s.glow_style == GlowStyle::Bloom {
        2.0
    } else {
        5.0
    };
    p[36] = [
        s.glow,
        s.glow_style.kernel_value() as f32,
        s.glow_range,
        s.glow_spread,
    ];
    p[37] = [
        s.glow_warmth,
        (base * (1.0 + s.glow_spread / 100.0) * scale)
            .round()
            .clamp(1.0, 64.0),
        0.0,
        0.0,
    ];
    p[38] = [
        s.vignette_amount,
        s.vignette_style.kernel_value() as f32,
        // The midpoint is measured against the frame's half-diagonal, and the
        // roundness is Compositor's blend between a circle and a square.
        s.vignette_midpoint / 100.0 * 0.85,
        (1.0 - s.vignette_roundness / 100.0) * 0.5,
    ];
    p[39] = [
        (s.vignette_feather / 100.0).max(0.05),
        s.vignette_highlights,
        0.0,
        0.0,
    ];
    p[40] = [
        s.grain_amount,
        0.5 + (s.grain_size / 100.0) * 19.5,
        (s.grain_roughness / 100.0).clamp(0.0, 1.0),
        if scale > 0.0 { 1.0 / scale } else { 1.0 },
    ];
    p[41] = [
        s.calibration.shadow_tint / 100.0,
        s.calibration.red_hue / 100.0,
        s.calibration.red_saturation / 100.0,
        s.calibration.process.kernel_value() as f32,
    ];
    p[42] = [
        s.calibration.green_hue / 100.0,
        s.calibration.green_saturation / 100.0,
        s.calibration.blue_hue / 100.0,
        s.calibration.blue_saturation / 100.0,
    ];
}

fn overlay_config(overlay: &Overlay, size: [u32; 2], cancel: &AtomicBool) -> Result<Vec<[f32; 4]>> {
    let mut p = vec![
        [size[0] as f32, size[1] as f32, 0.0, 0.0],
        [
            overlay.start.x,
            overlay.start.y,
            overlay.end.x,
            overlay.end.y,
        ],
        [
            match overlay.kind {
                OverlayKind::Linear => 0.0,
                OverlayKind::Radial => 1.0,
                OverlayKind::Brush => 2.0,
            },
            overlay.feather,
            if overlay.invert { 1.0 } else { 0.0 },
            overlay.radius,
        ],
        [overlay.exposure, overlay.warmth, overlay.saturation, 0.0],
        [size[0].div_ceil(32) as f32, 0.0, 0.0, 0.0],
    ];
    if overlay.kind == OverlayKind::Brush {
        // Bin dab centers once. Each GPU pixel visits only overlapping dabs.
        let grid = [size[0].div_ceil(32), size[1].div_ceil(32)];
        let mut tiles = vec![Vec::new(); (grid[0] * grid[1]) as usize];
        let radius = overlay.radius * size[1] as f32;
        // Bound the acceleration structure independently of image size. Very
        // broad, dense strokes must not exhaust host memory before GPU fallback.
        let mut records = 5 + tiles.len();
        for point in &overlay.points {
            ensure!(!cancel.load(Ordering::Relaxed), "RAW development cancelled");
            let x = point.x * size[0] as f32;
            let y = point.y * size[1] as f32;
            let left = ((x - radius).max(0.0) as u32 / 32).min(grid[0] - 1);
            let top = ((y - radius).max(0.0) as u32 / 32).min(grid[1] - 1);
            let right = ((x + radius).ceil() as u32 / 32).min(grid[0] - 1);
            let bottom = ((y + radius).ceil() as u32 / 32).min(grid[1] - 1);
            records += ((right - left + 1) * (bottom - top + 1)) as usize;
            ensure!(
                records <= 4_194_304,
                "RAW brush coverage exceeds the GPU tile budget"
            );
            for y in top..=bottom {
                for x in left..=right {
                    tiles[(y * grid[0] + x) as usize].push([point.x, point.y, 0.0, 0.0]);
                }
            }
        }
        p.resize(5 + tiles.len(), [0.0; 4]);
        for (i, tile) in tiles.into_iter().enumerate() {
            p[5 + i] = [p.len() as f32, tile.len() as f32, 0.0, 0.0];
            p.extend(tile);
        }
    }
    Ok(p)
}
