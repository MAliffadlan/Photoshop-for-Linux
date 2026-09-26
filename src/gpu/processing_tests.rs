use super::*;
use image::{Rgba, RgbaImage};

fn processor() -> Arc<Processor> {
    let instance = wgpu::Instance::new(&Default::default());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).unwrap();
    Processor::new(device, queue)
}
fn fixture(w: u32, h: u32) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, y| {
        Rgba([
            (x * 37 + y * 13) as u8,
            (x * 17 + y * 43) as u8,
            (x * 7 + y * 23) as u8,
            if x % 7 == 0 {
                0
            } else {
                (x * 31 + y * 53) as u8
            },
        ])
    })
}
/// A small RAW frame with a colour matrix, an as-shot white balance and a
/// greenish cast, so a develop that only tints can be told apart from one that
/// does nothing.
fn raw_fixture() -> crate::raw::DecodedRaw {
    use crate::raw::{DecodedRaw, RawMetadata};
    DecodedRaw {
        camera: image::Rgb32FImage::from_fn(89, 67, |x, y| {
            image::Rgb([
                0.03 + (x * x % 231) as f32 / 180.0,
                0.01 + (x * y % 137) as f32 / 100.0,
                0.02 + (y * y % 193) as f32 / 190.0,
            ])
        }),
        alpha: None,
        as_shot: [1.15, 1.0, 0.92],
        camera_to_rgb: [
            [1.1, -0.06, -0.04],
            [-0.07, 1.13, -0.06],
            [-0.04, -0.06, 1.1],
        ],
        xyz_to_camera: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        metadata: RawMetadata {
            width: 89,
            height: 67,
            ..Default::default()
        },
    }
}
fn compare(a: &RgbaImage, b: &RgbaImage, tolerance: u8) {
    assert_eq!(a.dimensions(), b.dimensions());
    let mut max = 0;
    for (a, b) in a.pixels().zip(b.pixels()) {
        for c in 0..4 {
            // Ignore invisible RGB, but compare straight colors for visible output.
            if c == 3 || a[3] > 0 || b[3] > 0 {
                max = max.max(a[c].abs_diff(b[c]));
            }
        }
    }
    if max > tolerance {
        for (i, (p, q)) in a.pixels().zip(b.pixels()).enumerate() {
            if p.0.iter().zip(q.0).any(|(p, q)| p.abs_diff(q) == max) {
                eprintln!(
                    "Worst pixel {},{}: {:?} vs {:?}",
                    i % a.width() as usize,
                    i / a.width() as usize,
                    p,
                    q
                );
                break;
            }
        }
    }
    assert!(max <= tolerance, "maximum error {max}, allowed {tolerance}");
}

#[test]
#[ignore = "requires native compute adapter"]
fn processing_filters_and_resampling_match_cpu() {
    let gpu = processor();
    let source = fixture(71, 53);
    for radius in [0.3, 1.0, 3.5, 12.0] {
        let bytes = gpu
            .separable(source.as_raw(), [71, 53], [71, 53], 0, Some(radius))
            .unwrap();
        let actual = RgbaImage::from_raw(71, 53, bytes).unwrap();
        let expected =
            crate::effects::filtered(&source, &crate::effects::Filter::GaussianBlur { radius });
        compare(&actual, &expected, 2);
    }
    for size in [[19, 27], [105, 87], [1, 9], [71, 53]] {
        let actual = RgbaImage::from_raw(
            size[0],
            size[1],
            gpu.separable(source.as_raw(), [71, 53], size, 0, None)
                .unwrap(),
        )
        .unwrap();
        compare(
            &actual,
            &crate::render::resize_quality(&source, size[0], size[1]),
            1,
        );
    }
    let source = fixture(257, 259);
    for filter in [
        crate::effects::Filter::Noise {
            amount: 37.0,
            monochrome: false,
        },
        crate::effects::Filter::LensCorrection {
            distortion: 27.0,
            vignette: 19.0,
        },
    ] {
        let actual = scope(Some(gpu.clone()), || {
            super::filter(&source, &filter).unwrap()
        });
        compare(&actual, &crate::effects::filtered(&source, &filter), 1);
    }
    for filter in [
        crate::effects::Filter::Vignette {
            amount: 73.0,
            color: [0.8, 0.2, 0.1],
            midpoint: 42.0,
            roundness: 35.0,
            feather: 48.0,
            highlights: 31.0,
        },
        crate::effects::Filter::BloomGlow {
            amount: 68.0,
            radius: 7.0,
        },
        crate::effects::Filter::TonalContrast {
            amount: 64.0,
            radius: 5.0,
            shadows: -20.0,
            midtones: 80.0,
            highlights: 15.0,
        },
    ] {
        let actual = scope(Some(gpu.clone()), || {
            super::filter(&source, &filter).unwrap()
        });
        compare(&actual, &crate::effects::filtered(&source, &filter), 3);
    }
}

#[test]
#[ignore = "requires native compute adapter"]
fn processing_adjustments_and_composition_match_cpu() {
    let gpu = processor();
    let source = fixture(71, 53);
    let transform = crate::document::Transform {
        x: 12.0,
        y: 7.0,
        rotation: 13.0,
        flip_x: true,
        ..crate::document::Transform::new(91, 87)
    };
    let selection =
        image::GrayImage::from_fn(120, 100, |x, y| image::Luma([(x * 17 + y * 19) as u8]));
    for adjustment in [
        Adjustment::HueSaturation {
            hue: 37.0,
            saturation: 23.0,
            lightness: -17.0,
            colorize: false,
        },
        Adjustment::HueSaturation {
            hue: 213.0,
            saturation: 63.0,
            lightness: 11.0,
            colorize: true,
        },
        Adjustment::HueRanges {
            settings: Box::new(crate::color::HueSettings {
                adjustments: [[13.0, 17.0, -7.0]; 7],
                invert_range: true,
                range: 2,
                ..Default::default()
            }),
        },
        Adjustment::Levels {
            black: 17.0,
            gamma: 1.3,
            white: 239.0,
            output_black: 7.0,
            output_white: 241.0,
        },
        Adjustment::LevelsChannels {
            ranges: [
                [13.0, 1.2, 245.0, 3.0, 247.0],
                [7.0, 0.9, 243.0, 11.0, 251.0],
                [19.0, 1.1, 237.0, 5.0, 233.0],
                [0.0, 1.0, 255.0, 0.0, 255.0],
            ],
        },
        Adjustment::CurvesChannels {
            channels: std::array::from_fn(|i| {
                vec![
                    Point::new(0.0, 0.0),
                    Point::new(0.4, 0.3 + i as f32 * 0.1),
                    Point::new(1.0, 1.0),
                ]
            }),
        },
        Adjustment::GradientMap {
            shadows: [17, 31, 53, 255],
            highlights: [237, 193, 149, 255],
        },
        Adjustment::Grain {
            amount: 27.0,
            monochrome: false,
            seed: 7931,
        },
        Adjustment::AddNoise {
            amount: 55.0,
            gaussian: false,
            monochromatic: false,
            seed: 313,
        },
        Adjustment::AddNoise {
            amount: 90.0,
            gaussian: true,
            monochromatic: true,
            seed: 5150,
        },
        Adjustment::Invert,
        Adjustment::Exposure {
            exposure: 0.7,
            offset: -0.02,
            gamma: 1.2,
        },
        Adjustment::FilmGrain {
            amount: 23.0,
            size: 2.7,
            roughness: 65.0,
            seed: 472,
        },
        Adjustment::Curves {
            points: vec![
                Point::new(0.0, 0.0),
                Point::new(0.4, 0.65),
                Point::new(1.0, 1.0),
            ],
        },
    ] {
        let mut document = Document::new(120, 100).unwrap();
        let mut layer = Layer::image("source", source.clone());
        layer.transform = transform;
        document.insert(layer);
        document.selection = Some(Arc::new(selection.clone()));
        let actual = gpu
            .adjustment(&source, &adjustment, transform, Some(&selection), false)
            .unwrap();
        crate::effects::apply_adjustment(&mut document, &adjustment, false).unwrap();
        compare(
            &actual,
            document.active().unwrap().pixels.as_ref().unwrap(),
            1,
        );
        compare(
            &gpu.compose(&document, 120, 100).unwrap(),
            &crate::render::render(&document),
            3,
        );
    }
}

#[test]
#[ignore = "requires native compute adapter"]
fn processing_raw_matches_cpu_at_both_depths() {
    use crate::raw::*;
    use std::sync::atomic::AtomicBool;
    let gpu = processor();
    let raw = raw_fixture();
    let mut settings = DevelopSettings {
        exposure: -0.3,
        brightness: 12.0,
        contrast: 17.0,
        highlights: -19.0,
        shadows: 23.0,
        whites: 11.0,
        blacks: -6.0,
        saturation: 13.0,
        vibrance: 21.0,
        clarity: 12.0,
        texture: 17.0,
        dehaze: 9.0,
        luminance_noise: 31.0,
        color_noise: 37.0,
        sharpen: 67.0,
        sharpen_threshold: 0.002,
        distortion: 11.0,
        chromatic_red: 13.0,
        chromatic_blue: -17.0,
        defringe: 27.0,
        vignette: -11.0,
        rotation: 7.0,
        perspective: [3.0, -5.0],
        crop: [0.07, 0.11, 0.94, 0.92],
        shadow_tone: [218.0, 11.0],
        highlight_tone: [37.0, 19.0],
        tone_balance: 13.0,
        ..Default::default()
    };
    settings.curves[1] = [0.03, 0.21, 0.55, 0.78, 0.97];
    settings.hsl[2] = [17.0, -23.0, 13.0];
    settings.overlays = vec![
        Overlay {
            exposure: 0.4,
            warmth: 13.0,
            saturation: -11.0,
            ..Default::default()
        },
        Overlay {
            kind: OverlayKind::Radial,
            invert: true,
            exposure: -0.7,
            ..Default::default()
        },
        Overlay {
            kind: OverlayKind::Brush,
            radius: 0.12,
            points: vec![
                Point::new(0.3, 0.2),
                Point::new(0.7, 0.8),
                Point::new(0.5, 0.5),
            ],
            exposure: 0.3,
            warmth: -19.0,
            ..Default::default()
        },
    ];
    let cancel = AtomicBool::new(false);
    for s in [
        DevelopSettings {
            sharpen: 0.0,
            color_noise: 0.0,
            ..Default::default()
        },
        DevelopSettings {
            sharpen: 0.0,
            ..Default::default()
        },
        DevelopSettings::default(),
        settings.clone(),
        DevelopSettings {
            monochrome: true,
            ..settings.clone()
        },
        DevelopSettings {
            grading: Grading {
                shadows: GradeWheel {
                    hue: 214.0,
                    saturation: 47.0,
                    luminance: -23.0,
                },
                midtones: GradeWheel {
                    hue: 43.0,
                    saturation: 31.0,
                    luminance: 17.0,
                },
                highlights: GradeWheel {
                    hue: 96.0,
                    saturation: 39.0,
                    luminance: 0.0,
                },
                global: GradeWheel {
                    hue: 318.0,
                    saturation: 13.0,
                    luminance: 7.0,
                },
                blending: 71.0,
                balance: -29.0,
            },
            ..settings.clone()
        },
    ] {
        let [l, t, r, b] = super::raw_crop(&s, [89, 67]);
        let actual = RgbaImage::from_raw(
            r - l,
            b - t,
            gpu.develop(&raw, &s, raw.as_shot, 8, &cancel).unwrap(),
        )
        .unwrap();
        compare(&actual, &crate::raw::render(&raw, &s, &cancel).unwrap(), 1);
        let actual = gpu.develop(&raw, &s, raw.as_shot, 16, &cancel).unwrap();
        let expected = crate::raw::render_16(&raw, &s, &cancel).unwrap();
        let error = actual
            .as_chunks::<2>()
            .0
            .iter()
            .zip(expected.as_raw())
            .map(|(a, &b)| u16::from_le_bytes(*a).abs_diff(b))
            .max()
            .unwrap();
        // A hard sharpening cutoff is discontinuous. Floating-point roundoff can
        // switch a boundary pixel; constrain both the magnitude and frequency.
        let outliers = actual
            .as_chunks::<2>()
            .0
            .iter()
            .zip(expected.as_raw())
            .filter(|(a, b)| u16::from_le_bytes(**a).abs_diff(**b) > 8)
            .count();
        assert!(
            error <= 8 || (s.sharpen_threshold > 0.0 && error <= 256 && outliers <= 8),
            "16-bit RAW error {error}, outliers {outliers}"
        );
    }
}

#[test]
#[ignore = "requires native compute adapter"]
fn processing_raw_grading_matches_cpu_at_both_depths() {
    use crate::raw::{GradeWheel, Grading, *};
    use std::sync::atomic::AtomicBool;
    let gpu = processor();
    let raw = raw_fixture();
    let cancel = AtomicBool::new(false);
    // The sharpening pass is left out on purpose: its hard cutoff makes a
    // software adapter disagree with the CPU at a few boundary pixels, which
    // would hide whether the grading itself agrees.
    let quiet = DevelopSettings {
        sharpen: 0.0,
        color_noise: 0.0,
        luminance_noise: 0.0,
        ..Default::default()
    };
    let wheel = |hue: f32, saturation: f32, luminance: f32| GradeWheel {
        hue,
        saturation,
        luminance,
    };
    for grading in [
        Grading::default(),
        Grading {
            shadows: wheel(214.0, 47.0, -23.0),
            blending: 50.0,
            balance: 0.0,
            ..Grading::default()
        },
        Grading {
            midtones: wheel(43.0, 31.0, 17.0),
            highlights: wheel(96.0, 39.0, 0.0),
            global: wheel(318.0, 13.0, 7.0),
            blending: 71.0,
            balance: -29.0,
            ..Grading::default()
        },
        Grading {
            shadows: wheel(0.0, 100.0, -60.0),
            highlights: wheel(120.0, 100.0, 60.0),
            blending: 0.0,
            balance: 100.0,
            ..Grading::default()
        },
    ] {
        let s = DevelopSettings {
            grading,
            ..quiet.clone()
        };
        let [l, t, r, b] = super::raw_crop(&s, [89, 67]);
        let actual = RgbaImage::from_raw(
            r - l,
            b - t,
            gpu.develop(&raw, &s, raw.as_shot, 8, &cancel).unwrap(),
        )
        .unwrap();
        let expected = crate::raw::render(&raw, &s, &cancel).unwrap();
        compare(&actual, &expected, 1);
        let actual = gpu.develop(&raw, &s, raw.as_shot, 16, &cancel).unwrap();
        let expected = crate::raw::render_16(&raw, &s, &cancel).unwrap();
        let diffs: Vec<u32> = actual
            .as_chunks::<2>()
            .0
            .iter()
            .zip(expected.as_raw())
            .map(|(a, &b)| u32::from(u16::from_le_bytes(*a).abs_diff(b)))
            .collect();
        // A wheel's luminance shift multiplies the pixel, and a software
        // adapter's divide lands a few steps away from the CPU's, so a midtone
        // can end up a little off where the CPU put it: a tenth of a percent of
        // full scale. Bound the magnitude and the frequency instead of letting
        // one rounding boundary fail the comparison, which is what would catch
        // a wrong weight or a wheel the shader skipped.
        let error = *diffs.iter().max().unwrap();
        let outliers = diffs.iter().filter(|diff| **diff > 64).count();
        assert!(
            error <= 64 && outliers <= 8,
            "16-bit grading error {error}, outliers {outliers}"
        );
    }
}

/// Glow, the post-crop vignette, grain and calibration, checked against the CPU
/// at both depths. Sharpening and the noise reduction are left out on purpose:
/// their boundaries are where a software adapter disagrees with the CPU, and
/// they would hide whether these agree.
#[test]
#[ignore = "requires native compute adapter"]
fn processing_raw_effects_and_calibration_match_cpu_at_both_depths() {
    use crate::raw::{Calibration, GlowStyle, ProcessVersion, VignetteStyle, *};
    use std::sync::atomic::AtomicBool;
    let gpu = processor();
    let raw = raw_fixture();
    let cancel = AtomicBool::new(false);
    let quiet = DevelopSettings {
        sharpen: 0.0,
        color_noise: 0.0,
        luminance_noise: 0.0,
        ..Default::default()
    };
    for settings in [
        DevelopSettings {
            glow: 70.0,
            glow_range: -20.0,
            glow_spread: 40.0,
            ..quiet.clone()
        },
        DevelopSettings {
            glow: 90.0,
            glow_style: GlowStyle::Bloom,
            glow_warmth: 60.0,
            ..quiet.clone()
        },
        DevelopSettings {
            glow: 90.0,
            glow_style: GlowStyle::Halation,
            glow_warmth: -60.0,
            ..quiet.clone()
        },
        DevelopSettings {
            vignette_amount: -70.0,
            vignette_roundness: -40.0,
            vignette_feather: 30.0,
            ..quiet.clone()
        },
        DevelopSettings {
            vignette_amount: -70.0,
            vignette_style: VignetteStyle::ColorPriority,
            vignette_highlights: 40.0,
            ..quiet.clone()
        },
        DevelopSettings {
            vignette_amount: 60.0,
            vignette_style: VignetteStyle::PaintOverlay,
            vignette_midpoint: 20.0,
            ..quiet.clone()
        },
        DevelopSettings {
            grain_amount: 80.0,
            grain_size: 35.0,
            grain_roughness: 70.0,
            ..quiet.clone()
        },
        DevelopSettings {
            calibration: Calibration {
                shadow_tint: 70.0,
                red_hue: 40.0,
                red_saturation: 60.0,
                green_saturation: -50.0,
                blue_hue: 30.0,
                ..Calibration::default()
            },
            ..quiet.clone()
        },
        DevelopSettings {
            calibration: Calibration {
                red_hue: 100.0,
                green_hue: -100.0,
                blue_saturation: 100.0,
                process: ProcessVersion::Version2,
                ..Calibration::default()
            },
            ..quiet
        },
    ] {
        let [l, t, r, b] = super::raw_crop(&settings, [89, 67]);
        let actual = RgbaImage::from_raw(
            r - l,
            b - t,
            gpu.develop(&raw, &settings, raw.as_shot, 8, &cancel)
                .unwrap(),
        )
        .unwrap();
        let expected = crate::raw::render(&raw, &settings, &cancel).unwrap();
        compare(&actual, &expected, 1);
        let actual = gpu
            .develop(&raw, &settings, raw.as_shot, 16, &cancel)
            .unwrap();
        let expected = crate::raw::render_16(&raw, &settings, &cancel).unwrap();
        let diffs: Vec<u32> = actual
            .as_chunks::<2>()
            .0
            .iter()
            .zip(expected.as_raw())
            .map(|(a, &b)| u32::from(u16::from_le_bytes(*a).abs_diff(b)))
            .collect();
        // A box blur and a few multiplies are where a shader and a CPU fall a
        // rounding step apart, so the bound is on the frequency as well as the
        // size: a wrong weight or a skipped wheel is off by far more.
        let error = *diffs.iter().max().unwrap();
        let outliers = diffs.iter().filter(|diff| **diff > 64).count();
        assert!(
            error <= 64 && outliers <= 8,
            "16-bit effects error {error}, outliers {outliers}"
        );
    }
}

#[test]
#[ignore = "requires native compute adapter"]
fn processing_paint_masks_shapes_and_selection_match_cpu() {
    use crate::paint::{self, GradientOptions, ShapeKind};
    let gpu = processor();
    let source = fixture(257, 259);
    let mut base = Document::new(300, 300).unwrap();
    base.insert(Layer::image("Pixels", source.clone()));
    base.active_mut().unwrap().transform.rotation = 9.0;
    base.selection = Some(Arc::new(image::GrayImage::from_fn(300, 300, |x, y| {
        image::Luma([(x * 17 + y * 13) as u8])
    })));
    for mask in [false, true] {
        for operation in 0..5 {
            let apply = |doc: &mut Document| match operation {
                0 | 1 => paint::fill(doc, [173, 53, 211, 157], operation == 1, mask).unwrap(),
                2 | 3 => paint::gradient(
                    doc,
                    Point::new(17.0, 33.0),
                    Point::new(247.0, 211.0),
                    GradientOptions {
                        foreground: [17, 83, 153, 197],
                        background: [231, 127, 73, 143],
                        opacity: 0.63,
                        radial: operation == 3,
                        mask_target: mask,
                    },
                )
                .unwrap(),
                _ => crate::effects::apply_adjustment(
                    doc,
                    &Adjustment::Exposure {
                        exposure: 0.4,
                        offset: 0.01,
                        gamma: 1.3,
                    },
                    mask,
                )
                .unwrap(),
            };
            let mut cpu = base.clone();
            apply(&mut cpu);
            let mut actual = base.clone();
            scope(Some(gpu.clone()), || apply(&mut actual));
            if mask {
                let a = &actual.active().unwrap().mask.as_ref().unwrap().pixels;
                let b = &cpu.active().unwrap().mask.as_ref().unwrap().pixels;
                assert!(
                    a.as_raw()
                        .iter()
                        .zip(b.as_raw())
                        .all(|(a, b)| a.abs_diff(*b) <= 1)
                );
            } else {
                compare(
                    actual.active().unwrap().pixels.as_ref().unwrap(),
                    cpu.active().unwrap().pixels.as_ref().unwrap(),
                    1,
                );
            }
        }
    }
    for (kind, line_width) in [
        (ShapeKind::Ellipse, 0.0),
        (ShapeKind::RoundedRectangle, 0.0),
        (ShapeKind::Line, 7.5),
    ] {
        let make = || {
            paint::shape(
                Point::default(),
                Point::new(257.0, 259.0),
                kind,
                [23, 67, 183, 211],
                17.5,
                line_width,
            )
            .unwrap()
        };
        let cpu = make();
        let actual = scope(Some(gpu.clone()), make);
        compare(
            actual.pixels.as_ref().unwrap(),
            cpu.pixels.as_ref().unwrap(),
            0,
        );
    }
    let cpu = crate::selection::wand(&source, Point::new(7.0, 11.0), 23, false);
    let actual = scope(Some(gpu), || {
        crate::selection::wand(&source, Point::new(7.0, 11.0), 23, false)
    });
    assert_eq!(cpu, actual);
}

#[test]
#[ignore = "requires native compute adapter"]
fn float_gaussian_matches_reference() {
    let gpu = processor();
    let source = image::Rgb32FImage::from_fn(89, 67, |x, y| {
        image::Rgb([
            0.03 + (x * x % 231) as f32 / 180.0,
            0.01 + (x * y % 137) as f32 / 100.0,
            0.02 + (y * y % 193) as f32 / 190.0,
        ])
    });
    for sigma in [0.3, 1.0, 3.0, 24.0] {
        let actual = gpu
            .separable(
                bytemuck::cast_slice(source.as_raw()),
                [89, 67],
                [89, 67],
                2,
                Some(sigma),
            )
            .unwrap();
        let expected = image::imageops::blur(&source, sigma);
        let error = actual
            .as_chunks::<4>()
            .0
            .iter()
            .zip(expected.as_raw())
            .map(|(a, b)| (f32::from_ne_bytes(*a) - b).abs())
            .fold(0.0, f32::max);
        assert!(error < 0.000002, "sigma {sigma}: error {error}");
    }
}

#[test]
#[ignore = "requires native compute adapter"]
fn processing_brushes_coverage_and_analysis_match_cpu() {
    use crate::paint::{self, Brush, PaintMode, StrokeOptions};
    let gpu = processor();
    let mut base = Document::new(320, 300).unwrap();
    base.insert(Layer::image("Pixels", fixture(320, 300)));
    base.selection = Some(Arc::new(image::GrayImage::from_fn(320, 300, |x, y| {
        image::Luma([(x * 19 + y * 11) as u8])
    })));
    let source = fixture(320, 300);
    let brush = Brush {
        diameter: 260.0,
        hardness: 0.6,
        opacity: 0.7,
        color: [137, 59, 213, 191],
        ..Brush::default()
    };
    for mask in [false, true] {
        for mode in [
            PaintMode::Paint,
            PaintMode::Erase,
            PaintMode::Clone,
            PaintMode::Blur,
            PaintMode::Heal,
            PaintMode::Smudge,
        ] {
            let apply = |doc: &mut Document| {
                paint::stroke(
                    doc,
                    Point::new(150.0, 140.0),
                    Point::new(167.0, 153.0),
                    &brush,
                    StrokeOptions {
                        mode,
                        mask_target: mask,
                        source: Some(&source),
                        clone_offset: Point::new(17.0, -13.0),
                    },
                )
                .unwrap()
            };
            let mut expected = base.clone();
            apply(&mut expected);
            let mut actual = base.clone();
            scope(Some(gpu.clone()), || apply(&mut actual));
            if mask {
                let a = &actual.active().unwrap().mask.as_ref().unwrap().pixels;
                let b = &expected.active().unwrap().mask.as_ref().unwrap().pixels;
                assert!(
                    a.as_raw()
                        .iter()
                        .zip(b.as_raw())
                        .all(|(a, b)| a.abs_diff(*b) <= 1)
                );
            } else {
                compare(
                    actual.active().unwrap().pixels.as_ref().unwrap(),
                    expected.active().unwrap().pixels.as_ref().unwrap(),
                    1,
                );
            }
        }
    }
    let mut group = Layer::blank("Group", 320, 300);
    group.group = true;
    group.opacity = 0.73;
    group.mask = Some(crate::document::Mask {
        pixels: Arc::new(image::GrayImage::from_fn(19, 13, |x, y| {
            image::Luma([(x * 7 + y * 11) as u8])
        })),
        enabled: true,
        linked: true,
        placement: Some(crate::document::Transform::new(300, 300)),
    });
    let mut clipped = Layer::image("Clipped", source.clone());
    clipped.parent = Some(group.id);
    clipped.clip_to = base.active;
    clipped.opacity = 0.61;
    clipped.mask = Some(crate::document::Mask {
        pixels: Arc::new(image::GrayImage::from_fn(23, 17, |x, y| {
            image::Luma([(x * 13 + y * 19) as u8])
        })),
        enabled: true,
        linked: true,
        placement: Some(crate::document::Transform {
            rotation: 7.0,
            ..crate::document::Transform::new(310, 280)
        }),
    });
    base.layers.push(group);
    base.layers.push(clipped);
    let expected = crate::render::render(&base);
    let actual = scope(Some(gpu.clone()), || gpu.compose(&base, 320, 300).unwrap());
    compare(&actual, &expected, 2);
    let expected = crate::effects::histogram(&source);
    let analysis = scope(Some(gpu), || super::analyze(&source, true).unwrap());
    assert_eq!(analysis.luminance, expected);
    let mut channels = [[0u32; 256]; 3];
    let mut counts = [0u32; 2];
    let mut visible = 0;
    let mut warnings = source.clone();
    for (p, w) in source.pixels().zip(warnings.pixels_mut()) {
        if p[3] == 0 {
            continue;
        }
        visible += 1;
        for c in 0..3 {
            channels[c][p[c] as usize] += 1;
        }
        if p.0[..3].contains(&255) {
            *w = Rgba([255, 35, 65, 255]);
            counts[1] += 1;
        } else if p.0[..3].iter().all(|&v| v <= 1) {
            *w = Rgba([40, 100, 255, 255]);
            counts[0] += 1;
        }
    }
    assert_eq!(analysis.channels, channels);
    assert_eq!(
        analysis.clipping,
        counts.map(|n| 100.0 * n as f32 / visible as f32)
    );
    assert_eq!(analysis.warnings.unwrap(), warnings);
}

#[test]
#[ignore = "native GPU timing; run explicitly with --nocapture"]
fn benchmark_processing_backends() {
    use std::time::Instant;
    let gpu = processor();
    let image = fixture(1600, 1200);
    let mut document = Document::new(1600, 1200).unwrap();
    document.insert(Layer::image("Photo", image.clone()));
    let measure = |name: &str, operation: &dyn Fn()| {
        operation();
        let mut times = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            operation();
            times.push(start.elapsed());
        }
        times.sort();
        eprintln!("{name}: {:.2} ms", times[1].as_secs_f64() * 1000.0);
    };
    for filter in [
        crate::effects::Filter::GaussianBlur { radius: 8.0 },
        crate::effects::Filter::Noise {
            amount: 31.0,
            monochrome: false,
        },
        crate::effects::Filter::LensCorrection {
            distortion: 23.0,
            vignette: 17.0,
        },
    ] {
        measure(&format!("{} CPU", filter.name()), &|| {
            std::hint::black_box(crate::effects::filtered(&image, &filter));
        });
        measure(&format!("{} GPU + transfers", filter.name()), &|| {
            scope(Some(gpu.clone()), || {
                std::hint::black_box(super::filter(&image, &filter).unwrap());
            });
        });
    }
    let adjustment = Adjustment::Exposure {
        exposure: 0.7,
        offset: 0.01,
        gamma: 1.2,
    };
    measure("Exposure CPU", &|| {
        let mut doc = document.clone();
        crate::effects::apply_adjustment(&mut doc, &adjustment, false).unwrap();
        std::hint::black_box(doc);
    });
    measure("Exposure GPU + transfers", &|| {
        std::hint::black_box(
            gpu.adjustment(
                &image,
                &adjustment,
                crate::document::Transform::new(1600, 1200),
                None,
                false,
            )
            .unwrap(),
        );
    });
    measure("Resize CPU", &|| {
        std::hint::black_box(crate::render::resize_quality(&image, 800, 600));
    });
    measure("Resize GPU + transfers", &|| {
        scope(Some(gpu.clone()), || {
            std::hint::black_box(super::resize_rgba(&image, 800, 600).unwrap());
        });
    });
    measure("Composition CPU", &|| {
        std::hint::black_box(crate::render::render(&document));
    });
    measure("Composition GPU + readback", &|| {
        std::hint::black_box(gpu.compose(&document, 1600, 1200).unwrap());
    });
    measure("Histogram CPU", &|| {
        std::hint::black_box(crate::effects::histogram(&image));
    });
    measure("Histogram GPU + transfers", &|| {
        scope(Some(gpu.clone()), || {
            std::hint::black_box(super::analyze(&image, false).unwrap());
        });
    });
    let raw = crate::raw::DecodedRaw {
        camera: image::Rgb32FImage::from_fn(1600, 1200, |x, y| {
            image::Rgb([x as f32 / 1300.0, y as f32 / 1500.0, 0.4])
        }),
        alpha: None,
        as_shot: [1.0; 3],
        camera_to_rgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        xyz_to_camera: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        metadata: crate::raw::RawMetadata {
            width: 1600,
            height: 1200,
            ..Default::default()
        },
    };
    let settings = crate::raw::DevelopSettings::default();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    measure("RAW CPU", &|| {
        std::hint::black_box(crate::raw::render(&raw, &settings, &cancel).unwrap());
    });
    measure("RAW GPU + transfers", &|| {
        std::hint::black_box(
            gpu.develop(&raw, &settings, raw.as_shot, 8, &cancel)
                .unwrap(),
        );
    });
}

#[test]
#[ignore = "requires native compute adapter"]
fn processing_selection_projection_and_alpha_baking_match_cpu() {
    let gpu = processor();
    let mut document = Document::new(320, 300).unwrap();
    let mut layer = Layer::image("Pixels", fixture(317, 271));
    layer.opacity = 0.73;
    layer.transform.rotation = 11.0;
    layer.transform.warp = Some([
        Point::new(0.03, 0.02),
        Point::new(0.95, 0.0),
        Point::new(1.0, 0.97),
        Point::new(0.0, 1.0),
    ]);
    layer.mask = Some(crate::document::Mask {
        pixels: Arc::new(image::GrayImage::from_fn(17, 19, |x, y| {
            image::Luma([(x * 17 + y * 11) as u8])
        })),
        ..crate::document::Mask::white()
    });
    document.insert(layer.clone());
    document.selection = Some(Arc::new(image::GrayImage::from_fn(320, 300, |x, y| {
        image::Luma([(x * 29 + y * 13) as u8])
    })));
    for mask in [false, true] {
        let mut expected = document.clone();
        crate::operations::selection_from_layer(&mut expected, mask);
        let mut actual = document.clone();
        scope(Some(gpu.clone()), || {
            crate::operations::selection_from_layer(&mut actual, mask)
        });
        assert!(
            expected
                .selection
                .unwrap()
                .as_raw()
                .iter()
                .zip(actual.selection.unwrap().as_raw())
                .all(|(a, b)| a.abs_diff(*b) <= 1)
        );
    }
    let expected = crate::paint::mask_from_selection(&document, &layer);
    let actual = scope(Some(gpu.clone()), || {
        crate::paint::mask_from_selection(&document, &layer)
    });
    for (x, y, pixel) in actual.enumerate_pixels() {
        if pixel[0].abs_diff(expected.get_pixel(x, y)[0]) > 1 {
            let point = layer.transform.point(Point::new(
                (x as f32 + 0.5) / 317.0,
                (y as f32 + 0.5) / 271.0,
            ));
            let boundary = (point.x - point.x.round())
                .abs()
                .min((point.y - point.y.round()).abs());
            assert!(
                boundary < 0.0001,
                "Selection differs away from pixel boundary at {x},{y}: {point:?}"
            );
        }
    }
    let original = fixture(300, 300);
    let transform = crate::document::Transform {
        rotation: -9.0,
        ..crate::document::Transform::new(270, 260)
    };
    let expected = RgbaImage::from_fn(300, 300, |x, y| {
        let mut p = *original.get_pixel(x, y);
        let point = transform.point(Point::new(
            (x as f32 + 0.5) / 300.0,
            (y as f32 + 0.5) / 300.0,
        ));
        p[3] =
            (p[3] as f32 * crate::render::layer_alpha(&document, &layer, point, 0)).round() as u8;
        p
    });
    let actual = scope(Some(gpu.clone()), || {
        super::bake_alpha(&document, &layer, &original, transform).unwrap()
    });
    compare(&actual, &expected, 1);
    let mask = image::GrayImage::from_fn(317, 271, |x, y| image::Luma([(x * 11 + y * 31) as u8]));
    let expected = image::GrayImage::from_fn(317, 271, |x, y| {
        let point = layer.transform.point(Point::new(
            (x as f32 + 0.5) / 317.0,
            (y as f32 + 0.5) / 271.0,
        ));
        image::Luma([
            (mask.get_pixel(x, y)[0] as f32 * crate::render::own_mask(&layer, point)).round() as u8,
        ])
    });
    let actual = scope(Some(gpu), || super::bake_mask(&layer, &mask).unwrap());
    assert!(
        expected
            .as_raw()
            .iter()
            .zip(actual.as_raw())
            .all(|(a, b)| a.abs_diff(*b) <= 1)
    );
}

/// The blur and noise adjustment layers Compositor 1.2.3 added. A blur redraws the backdrop, so the
/// GPU runs it as passes of its own between the layers below and the layers above, and the CPU
/// renderer in `render.rs` is the reference it has to answer to.
#[test]
#[ignore = "requires native compute adapter"]
fn processing_backdrop_blurs_and_noise_match_cpu() {
    let gpu = processor();
    let mut base = Document::new(96, 72).unwrap();
    let mut layer = Layer::image("Base", fixture(96, 72));
    layer.transform = crate::document::Transform::new(96, 72);
    base.insert(layer);
    let mut patch = Layer::image("Patch", fixture(31, 19));
    patch.transform.x = 38.0;
    patch.transform.y = 27.0;
    base.layers.push(patch);

    for adjustment in [
        Adjustment::GaussianBlur { radius: 6.5 },
        Adjustment::GaussianBlur { radius: 0.4 },
        Adjustment::GaussianBlur { radius: 40.0 },
        Adjustment::MotionBlur {
            angle: 33.0,
            distance: 12.0,
        },
        Adjustment::MotionBlur {
            angle: -90.0,
            distance: 1.0,
        },
        Adjustment::AddNoise {
            amount: 45.0,
            gaussian: false,
            monochromatic: false,
            seed: 4729,
        },
        Adjustment::AddNoise {
            amount: 120.0,
            gaussian: true,
            monochromatic: true,
            seed: 77,
        },
    ] {
        let mut document = base.clone();
        let mut adjustment_layer = Layer::blank("Adjustment", 96, 72);
        adjustment_layer.adjustment = Some(adjustment.clone());
        document.layers.push(adjustment_layer);
        compare(
            &gpu.compose(&document, 96, 72).unwrap(),
            &crate::render::render(&document),
            3,
        );
    }

    // A blur layer answers for the layers beneath it only, so a layer with a mask and less than full
    // opacity above it has to leave the same picture behind on both renderers.
    let mut document = base.clone();
    let mut blurred = Layer::blank("Adjustment", 96, 72);
    blurred.adjustment = Some(Adjustment::GaussianBlur { radius: 5.0 });
    blurred.opacity = 0.75;
    document.layers.push(blurred);
    let mut top = Layer::image("Top", fixture(96, 72));
    top.transform = crate::document::Transform::new(96, 72);
    top.opacity = 0.5;
    top.mask = Some(crate::document::Mask {
        pixels: Arc::new(image::GrayImage::from_fn(96, 72, |x, y| {
            image::Luma([(x * 5 + y * 7) as u8])
        })),
        ..crate::document::Mask::white()
    });
    document.layers.push(top);
    compare(
        &gpu.compose(&document, 96, 72).unwrap(),
        &crate::render::render(&document),
        3,
    );
}

#[test]
#[ignore = "requires native compute adapter"]
fn processing_limits_cancellation_and_worker_context() {
    use std::sync::atomic::AtomicBool;
    let instance = wgpu::Instance::new(&Default::default());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits {
            max_storage_buffer_binding_size: 1024,
            ..Default::default()
        },
        ..Default::default()
    }))
    .unwrap();
    let gpu = Processor::new(device, queue);
    let image = fixture(257, 259);
    let filter = crate::effects::Filter::GaussianBlur { radius: 2.0 };
    let expected = crate::effects::filtered(&image, &filter);
    let actual = scope(Some(gpu.clone()), || {
        crate::effects::filtered(&image, &filter)
    });
    assert_eq!(expected, actual);
    assert!(current().is_none());
    scope(Some(gpu.clone()), || {
        let expected = gpu.clone();
        spawn(move || assert!(Arc::ptr_eq(&current().unwrap(), &expected)))
            .join()
            .unwrap();
        scope(None, || assert!(current().is_none()));
        assert!(Arc::ptr_eq(&current().unwrap(), &gpu));
    });
    assert!(current().is_none());
    let mut document = Document::new(257, 259).unwrap();
    document.insert(Layer::image("Pixels", image));
    let original = document.active().unwrap().pixels.clone();
    let result = scope(Some(gpu), || {
        crate::effects::apply_filter_cancellable(
            &mut document,
            &filter,
            false,
            &AtomicBool::new(true),
        )
    });
    assert!(result.is_err());
    assert_eq!(document.active().unwrap().pixels, original);
}

#[test]
#[ignore = "requires native compute adapter"]
fn processing_filter_apply_preserves_selections_bounds_masks_and_history_pixels() {
    let gpu = processor();
    let mut original = Document::new(320, 300).unwrap();
    let mut layer = Layer::image("Pixels", fixture(257, 259));
    layer.transform.x = 19.0;
    layer.transform.y = 17.0;
    layer.mask = Some(crate::document::Mask {
        pixels: Arc::new(image::GrayImage::from_fn(257, 259, |x, y| {
            image::Luma([(x * 17 + y * 23) as u8])
        })),
        ..crate::document::Mask::white()
    });
    original.insert(layer);
    original.selection = Some(Arc::new(image::GrayImage::from_fn(320, 300, |x, y| {
        image::Luma([(x * 11 + y * 17) as u8])
    })));
    let saved = original.active().unwrap().pixels.clone().unwrap();
    for filter in [
        crate::effects::Filter::GaussianBlur { radius: 3.0 },
        crate::effects::Filter::MotionBlur {
            distance: 17.0,
            angle: 31.0,
        },
        crate::effects::Filter::Noise {
            amount: 23.0,
            monochrome: true,
        },
        crate::effects::Filter::LensCorrection {
            distortion: 19.0,
            vignette: 13.0,
        },
    ] {
        let mut expected = original.clone();
        crate::effects::apply_filter(&mut expected, &filter, false).unwrap();
        let mut actual = original.clone();
        scope(Some(gpu.clone()), || {
            crate::effects::apply_filter(&mut actual, &filter, false)
        })
        .unwrap();
        compare(
            actual.active().unwrap().pixels.as_ref().unwrap(),
            expected.active().unwrap().pixels.as_ref().unwrap(),
            2,
        );
        assert_eq!(
            actual.active().unwrap().transform,
            expected.active().unwrap().transform
        );
        assert_eq!(
            actual.active().unwrap().mask.as_ref().unwrap().placement,
            expected.active().unwrap().mask.as_ref().unwrap().placement
        );
        assert!(Arc::ptr_eq(
            &saved,
            original.active().unwrap().pixels.as_ref().unwrap()
        ));
    }
    let filter = crate::effects::Filter::GaussianBlur { radius: 2.5 };
    let mut expected = original.clone();
    crate::effects::apply_filter(&mut expected, &filter, true).unwrap();
    let mut actual = original.clone();
    scope(Some(gpu), || {
        crate::effects::apply_filter(&mut actual, &filter, true)
    })
    .unwrap();
    assert!(
        expected
            .active()
            .unwrap()
            .mask
            .as_ref()
            .unwrap()
            .pixels
            .as_raw()
            .iter()
            .zip(
                actual
                    .active()
                    .unwrap()
                    .mask
                    .as_ref()
                    .unwrap()
                    .pixels
                    .as_raw()
            )
            .all(|(a, b)| a.abs_diff(*b) <= 1)
    );
}
