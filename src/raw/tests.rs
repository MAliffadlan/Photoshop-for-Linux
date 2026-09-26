use super::*;
use image::{Rgb, Rgba};
use std::sync::atomic::AtomicBool;

fn synthetic() -> DecodedRaw {
    DecodedRaw {
        camera: Rgb32FImage::from_fn(64, 48, |x, y| {
            Rgb([0.02 + x as f32 / 40.0, 0.02 + y as f32 / 30.0, 0.2])
        }),
        alpha: None,
        as_shot: [1.0; 3],
        camera_to_rgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        xyz_to_camera: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        metadata: RawMetadata {
            width: 64,
            height: 48,
            ..Default::default()
        },
    }
}

#[test]
fn exposure_recovers_unclipped_raw_values_and_is_repeatable() {
    let raw = synthetic();
    let cancel = AtomicBool::new(false);
    let mut settings = DevelopSettings {
        sharpen: 0.0,
        color_noise: 0.0,
        ..Default::default()
    };
    let before = render(&raw, &settings, &cancel).unwrap();
    settings.exposure = -2.0;
    let recovered = render(&raw, &settings, &cancel).unwrap();
    assert_eq!(before.get_pixel(63, 47)[0], 255);
    assert!(recovered.get_pixel(63, 47)[0] < 230);
    assert!(recovered.get_pixel(63, 47)[0] > recovered.get_pixel(55, 47)[0]);
    assert_eq!(recovered, render(&raw, &settings, &cancel).unwrap());
    assert!(raw.camera.get_pixel(63, 47)[0] > 1.0);
}

#[test]
fn validates_settings_and_cancellation() {
    let raw = synthetic();
    let mut s = DevelopSettings {
        exposure: f32::NAN,
        ..Default::default()
    };
    assert!(s.validate().is_err());
    s = DevelopSettings::default();
    s.crop = [0.5, 0.0, 0.4, 1.0];
    assert!(s.validate().is_err());
    assert!(render(&raw, &DevelopSettings::default(), &AtomicBool::new(true)).is_err());
    assert!(decode(b"broken camera file").is_err());
    assert!(is_raw(Path::new("PHOTO.NEF")));
    assert!(!is_raw(Path::new("photo.tiff")));
}

/// Compositor 1.2.1 opens Canon, Nikon, Sony, Fujifilm, DNG and more, so the extension gate now
/// covers every format rawler decodes rather than Nikon alone.
#[test]
fn raw_extensions_cover_the_formats_compositor_opens() {
    for name in [
        "photo.cr2",
        "photo.CR3",
        "photo.crm",
        "photo.crw",
        "photo.arw",
        "photo.ari",
        "photo.raf",
        "photo.rw2",
        "photo.dng",
        "photo.orf",
        "photo.pef",
        "photo.srw",
        "photo.mrw",
        "photo.erf",
        "photo.mef",
        "photo.iiq",
        "photo.kdc",
        "photo.dcr",
        "photo.dcs",
        "photo.mos",
        "photo.qtk",
        "photo.tfr",
        "photo.x3f",
        "photo.nrw",
    ] {
        assert!(is_raw(Path::new(name)), "{name} should open in Develop");
    }
    // Anything rawler has no decoder for stays an ordinary image, and a missing extension is fine.
    for name in ["photo.png", "photo.jpg", "photo.tiff", "photo", "photo.raw"] {
        assert!(!is_raw(Path::new(name)), "{name} is not a camera RAW");
    }
}

#[test]
fn crop_and_local_adjustment_are_nondestructive() {
    let raw = synthetic();
    let mut s = DevelopSettings {
        sharpen: 0.0,
        color_noise: 0.0,
        ..Default::default()
    };
    let cancel = AtomicBool::new(false);
    let before = render(&raw, &s, &cancel).unwrap();
    s.overlays.push(Overlay {
        exposure: -2.0,
        start: crate::document::Point::new(0.0, 0.0),
        end: crate::document::Point::new(0.0, 0.5),
        ..Default::default()
    });
    let after = render(&raw, &s, &cancel).unwrap();
    assert!(after.get_pixel(20, 0)[0] < before.get_pixel(20, 0)[0]);
    assert_eq!(after.get_pixel(20, 47), before.get_pixel(20, 47));
    s.crop = [0.25, 0.25, 0.75, 0.75];
    assert_eq!(render(&raw, &s, &cancel).unwrap().dimensions(), (32, 24));
}

#[test]
#[ignore = "Set MECTOV_TEST_NEF to a local camera file"]
fn sample_nef_develop_roundtrip() {
    let path = std::env::var_os("MECTOV_TEST_NEF").expect("Set MECTOV_TEST_NEF");
    let (asset, raw) = open(Path::new(&path)).unwrap();
    println!(
        "Camera metadata: {:?}; white balance: {:?}",
        raw.metadata, raw.as_shot
    );
    let proxy = raw.preview(1400);
    let pixels = render(&proxy, &asset.settings, &AtomicBool::new(false)).unwrap();
    if let Some(output) = std::env::var_os("MECTOV_TEST_RAW_PREVIEW") {
        pixels.save(output).unwrap();
    }
    assert!(pixels.pixels().any(|p| p[0] != p[1]));
    let full = render(&raw, &asset.settings, &AtomicBool::new(false)).unwrap();
    assert_eq!(full.dimensions(), (raw.metadata.width, raw.metadata.height));
    let mut layer = crate::document::Layer::image("RAW", full);
    layer.raw = Some(asset.clone());
    let mut document =
        crate::document::Document::new(raw.metadata.width, raw.metadata.height).unwrap();
    document.select(layer.id, false);
    document.layers = vec![layer];
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("raw.mectov");
    crate::io::save(&document, &project).unwrap();
    let loaded = crate::io::load(&project).unwrap();
    let restored = loaded.layers[0].raw.as_ref().unwrap();
    assert_eq!(restored.bytes, asset.bytes);
    assert_eq!(restored.settings, asset.settings);
    assert_eq!(loaded.layers[0].pixels, document.layers[0].pixels);
    let reopened = decode(&restored.bytes).unwrap();
    assert_eq!(
        render(
            &reopened.preview(1400),
            &restored.settings,
            &AtomicBool::new(false)
        )
        .unwrap(),
        pixels
    );
}

#[test]
fn sixteen_bit_output_preserves_more_than_eight_bit_steps() {
    let mut raw = synthetic();
    raw.camera = Rgb32FImage::from_fn(1024, 2, |x, _| Rgb([0.1 + x as f32 / 100_000.0; 3]));
    raw.metadata.width = 1024;
    raw.metadata.height = 2;
    let s = DevelopSettings {
        color_noise: 0.0,
        sharpen: 0.0,
        ..Default::default()
    };
    let cancel = AtomicBool::new(false);
    let image = render_16(&raw, &s, &cancel).unwrap();
    let steps: std::collections::HashSet<_> = image.pixels().map(|p| p[0]).collect();
    assert!(steps.len() > 900);
    let mut bytes = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgba16(image.clone())
        .write_to(&mut bytes, image::ImageFormat::Tiff)
        .unwrap();
    let restored = image::load_from_memory(bytes.get_ref()).unwrap();
    assert_eq!(restored.color(), image::ColorType::Rgba16);
    assert_eq!(restored.to_rgba16(), image);
}

#[test]
fn raw_project_assets_settings_and_history_survive_roundtrip() {
    let raw = synthetic();
    let mut asset = RawAsset {
        filename: "test.NEF".into(),
        metadata: raw.metadata.clone(),
        settings: DevelopSettings {
            exposure: -0.5,
            ..Default::default()
        },
        bytes: Arc::new(b"test fixture bytes".to_vec()),
    };
    asset.settings.overlays.push(Overlay {
        kind: OverlayKind::Brush,
        points: vec![crate::document::Point::new(0.5, 0.5)],
        ..Default::default()
    });
    let mut layer = crate::document::Layer::image(
        "RAW",
        render(&raw, &asset.settings, &AtomicBool::new(false)).unwrap(),
    );
    layer.raw = Some(asset.clone());
    layer.transform.x = 20.0;
    layer.opacity = 0.6;
    layer.mask = Some(crate::document::Mask::white());
    let mut doc = crate::document::Document::new(64, 48).unwrap();
    doc.select(layer.id, false);
    doc.layers = vec![layer];
    let mut history = crate::history::History::default();
    let before = doc.clone();
    history.begin("Develop RAW", &doc);
    asset.settings.exposure = 1.0;
    let pixels = render(&raw, &asset.settings, &AtomicBool::new(false)).unwrap();
    update_layer(&mut doc.layers[0], asset.clone(), pixels).unwrap();
    history.commit();
    assert_eq!(doc.layers[0].id, before.layers[0].id);
    assert_eq!(doc.layers[0].transform, before.layers[0].transform);
    assert_eq!(doc.layers[0].opacity, 0.6);
    assert!(doc.layers[0].mask.is_some());
    assert!(history.undo(&mut doc));
    assert_eq!(doc.layers[0].pixels, before.layers[0].pixels);
    assert_eq!(doc.layers[0].raw.as_ref().unwrap().settings.exposure, -0.5);
    assert!(history.redo(&mut doc));
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("raw.mectov");
    crate::io::save(&doc, &path).unwrap();
    let restored = crate::io::load(&path).unwrap();
    let restored_raw = restored.layers[0].raw.as_ref().unwrap();
    assert_eq!(restored_raw.bytes, asset.bytes);
    assert_eq!(restored_raw.settings, asset.settings);
    assert!(crate::paint::ensure_pixels(&mut doc.layers[0]).is_err());
    assert!(crate::paint::prepare_mask(&mut doc.layers[0]).is_ok());
    crate::operations::duplicate(&mut doc);
    assert!(Arc::ptr_eq(
        &doc.layers[0].raw.as_ref().unwrap().bytes,
        &doc.layers[1].raw.as_ref().unwrap().bytes
    ));
}

#[test]
fn corrupted_project_raw_sources_and_settings_are_rejected() {
    let raw = synthetic();
    let mut doc = crate::document::Document::new(64, 48).unwrap();
    doc.layers[0].pixels = Some(Arc::new(RgbaImage::new(64, 48)));
    doc.layers[0].raw = Some(RawAsset {
        filename: "test.NEF".into(),
        metadata: raw.metadata,
        settings: DevelopSettings::default(),
        bytes: Arc::new(vec![1, 2, 3]),
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("good.mectov");
    crate::io::save(&doc, &path).unwrap();
    let mut original = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
    let damaged = dir.path().join("missing.mectov");
    let mut out = zip::ZipWriter::new(File::create(&damaged).unwrap());
    for i in 0..original.len() {
        let file = original.by_index(i).unwrap();
        if !file.name().starts_with("raw/") {
            out.raw_copy_file(file).unwrap();
        }
    }
    out.finish().unwrap();
    assert!(
        crate::io::load(&damaged)
            .unwrap_err()
            .to_string()
            .contains("Missing project asset")
    );
    doc.layers[0].raw.as_mut().unwrap().settings.crop = [0.0; 4];
    assert!(crate::io::save(&doc, &path).is_err());
}

#[test]
fn raw_crop_preserves_the_placement_of_surviving_pixels() {
    let raw = synthetic();
    let asset = RawAsset {
        filename: "test.NEF".into(),
        metadata: raw.metadata,
        settings: DevelopSettings::default(),
        bytes: Arc::new(vec![1]),
    };
    let mut layer = crate::document::Layer::image("RAW", RgbaImage::new(64, 48));
    layer.raw = Some(asset.clone());
    layer.transform.rotation = 25.0;
    layer.transform.x = 50.0;
    let center_before = layer
        .transform
        .point(crate::document::Point::new(0.375, 0.5));
    let mut cropped = asset;
    cropped.settings.crop = [0.25, 0.0, 0.75, 1.0];
    update_layer(&mut layer, cropped, RgbaImage::new(32, 48)).unwrap();
    let center_after = layer
        .transform
        .point(crate::document::Point::new(0.25, 0.5));
    assert!(center_before.distance(center_after) < 0.001);
}

#[test]
fn the_camera_raw_filter_leaves_an_untouched_image_alone() {
    // Compositor's own filter holds to this: its default settings change nothing.
    let pixels = RgbaImage::from_fn(37, 23, |x, y| {
        let checker = (((x / 4 + y / 4) % 2) as u16) * 90;
        Rgba([
            (20 + checker + x as u16 * 5).min(255) as u8,
            (90 + y as u16 * 4).min(255) as u8,
            (200 - x as u16 * 2) as u8,
            255,
        ])
    });
    let asked = DevelopSettings {
        sharpen: 0.0,
        color_noise: 0.0,
        luminance_noise: 0.0,
        ..Default::default()
    };
    let result = filter::render_filter(&pixels, &asked, 0.0, 0.0, &AtomicBool::new(false)).unwrap();
    assert_eq!(result.dimensions(), pixels.dimensions());
    let mut worst = 0_i32;
    for (before, after) in pixels.pixels().zip(result.pixels()) {
        assert_eq!(before[3], after[3], "alpha is untouched");
        for c in 0..3 {
            worst = worst.max((i32::from(after[c]) - i32::from(before[c])).abs());
        }
    }
    assert!(
        worst <= 1,
        "the round trip through linear light moved a pixel by {worst}"
    );
}

#[test]
fn the_camera_raw_filter_keeps_a_layers_transparency() {
    // A soft edge is the case a camera file never has. The engine premultiplies
    // on the way in and divides back out on the way out, so the edge stays soft
    // and a straight render comes back with the colour it started with, instead
    // of fringing towards the transparent side.
    let soft = |x: u32, y: u32| -> u8 {
        let edge = (4.min(x) + 4.min(y) + 4.min(24 - 1 - x) + 4.min(24 - 1 - y)).min(8);
        ((edge * 255) / 8) as u8
    };
    let pixels = RgbaImage::from_fn(24, 24, |x, y| Rgba([220, 120, 40, soft(x, y)]));
    let asked = DevelopSettings {
        sharpen: 0.0,
        color_noise: 0.0,
        luminance_noise: 0.0,
        ..Default::default()
    };
    let untouched =
        filter::render_filter(&pixels, &asked, 0.0, 0.0, &AtomicBool::new(false)).unwrap();
    for (before, after) in pixels.pixels().zip(untouched.pixels()) {
        assert_eq!(before[3], after[3], "a soft edge stays soft");
        if before[3] > 0 {
            for c in 0..3 {
                assert!(
                    (i32::from(after[c]) - i32::from(before[c])).abs() <= 2,
                    "the straight colour survives the round trip: {before:?} -> {after:?}"
                );
            }
        }
    }
    // Clarity reads neighbouring pixels, so it works on a premultiplied field
    // whose alpha is a gradient. The opaque middle has no gradient to read, so
    // its colour is the one the layer started with.
    let mut sharpened = asked.clone();
    sharpened.clarity = 80.0;
    let developed =
        filter::render_filter(&pixels, &sharpened, 0.0, 0.0, &AtomicBool::new(false)).unwrap();
    for (before, after) in untouched.pixels().zip(developed.pixels()) {
        assert_eq!(before[3], after[3], "clarity does not touch alpha either");
    }
    let solid: Vec<&Rgba<u8>> = developed.pixels().filter(|pixel| pixel[3] == 255).collect();
    assert!(
        solid.len() > 100,
        "the test image has a solid middle: {}",
        solid.len()
    );
    for (channel, start) in [220_i32, 120, 40].into_iter().enumerate() {
        let mean = solid
            .iter()
            .map(|pixel| i32::from(pixel[channel]))
            .sum::<i32>()
            / solid.len() as i32;
        assert!(
            (mean - start).abs() <= 25,
            "channel {channel} averaged {mean} against {start}"
        );
    }
}

#[test]
fn the_camera_raw_filter_answers_its_own_controls() {
    let pixels = RgbaImage::from_fn(16, 16, |x, y| {
        Rgba([
            (40 + x as u16 * 12).min(255) as u8,
            (90 + y as u16 * 8).min(255) as u8,
            140,
            255,
        ])
    });
    let asked = DevelopSettings {
        sharpen: 0.0,
        color_noise: 0.0,
        luminance_noise: 0.0,
        ..Default::default()
    };
    let base = filter::render_filter(&pixels, &asked, 0.0, 0.0, &AtomicBool::new(false)).unwrap();
    let brighter =
        filter::render_filter(&pixels, &asked, 0.0, 0.0, &AtomicBool::new(false)).unwrap();
    assert_eq!(base, brighter, "the same settings render the same pixels");

    let mut lifted = asked.clone();
    lifted.exposure = 1.0;
    let exposed =
        filter::render_filter(&pixels, &lifted, 0.0, 0.0, &AtomicBool::new(false)).unwrap();
    assert!(
        exposed
            .pixels()
            .zip(base.pixels())
            .any(|(a, b)| a[0] > b[0]),
        "a stop of exposure brightens the image"
    );

    let mut monochrome = asked.clone();
    monochrome.monochrome = true;
    monochrome.bw_mix = [1.0, 1.0, 1.0];
    let grey =
        filter::render_filter(&pixels, &monochrome, 0.0, 0.0, &AtomicBool::new(false)).unwrap();
    for pixel in grey.pixels() {
        let spread = (i32::from(pixel[0]) - i32::from(pixel[1])).abs()
            + (i32::from(pixel[1]) - i32::from(pixel[2])).abs();
        assert!(spread < 24, "monochrome is grey: {pixel:?}");
    }

    let warm = filter::render_filter(&pixels, &asked, 100.0, 0.0, &AtomicBool::new(false)).unwrap();
    let red = warm
        .pixels()
        .zip(base.pixels())
        .filter(|(a, b)| a[0].abs_diff(b[0]) > 2)
        .count();
    let blue = warm
        .pixels()
        .zip(base.pixels())
        .filter(|(a, b)| a[2].abs_diff(b[2]) > 2)
        .count();
    assert!(
        red > 200 && blue > 200,
        "both channels move with the temperature"
    );
}

#[test]
fn a_cancelled_camera_raw_filter_says_so() {
    let pixels = RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 255]));
    let asked = DevelopSettings {
        sharpen: 0.0,
        color_noise: 0.0,
        luminance_noise: 0.0,
        ..Default::default()
    };
    let cancel = AtomicBool::new(true);
    assert!(filter::render_filter(&pixels, &asked, 0.0, 0.0, &cancel).is_err());
}

#[test]
fn a_neutral_grading_changes_nothing_and_says_so() {
    let pixels = RgbaImage::from_fn(24, 16, |x, y| {
        Rgba([
            (30 + x as u16 * 9).min(255) as u8,
            (70 + y as u16 * 10).min(255) as u8,
            150,
            255,
        ])
    });
    let asked = DevelopSettings {
        sharpen: 0.0,
        color_noise: 0.0,
        luminance_noise: 0.0,
        ..Default::default()
    };
    assert!(!asked.grading.adjusts(), "the default grading is neutral");
    let plain = filter::render_filter(&pixels, &asked, 0.0, 0.0, &AtomicBool::new(false)).unwrap();
    let graded = filter::render_filter(
        &pixels,
        &DevelopSettings {
            grading: Grading {
                shadows: GradeWheel {
                    hue: 210.0,
                    saturation: 40.0,
                    luminance: -20.0,
                },
                ..Grading::default()
            },
            ..asked.clone()
        },
        0.0,
        0.0,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(graded != plain, "a grading wheel changes the image");
    // The dark end of the ramp moves, because the shadow wheel owns it.
    let (dark, light) = (plain.get_pixel(0, 0), plain.get_pixel(23, 15));
    let (dark_graded, light_graded) = (graded.get_pixel(0, 0), graded.get_pixel(23, 15));
    assert!(
        luminance_of(dark_graded) < luminance_of(dark) - 1,
        "a blue shadow wheel darkens the shadows: {dark:?} -> {dark_graded:?}"
    );
    assert!(
        light_graded
            .0
            .iter()
            .zip(light.0)
            .all(|(after, before)| (i32::from(*after) - i32::from(before)).abs() <= 1),
        "and the highlight end belongs to the other wheels: {light:?} -> {light_graded:?}"
    );
}

fn luminance_of(pixel: &Rgba<u8>) -> i32 {
    2126 * i32::from(pixel[0]) + 7152 * i32::from(pixel[1]) + 722 * i32::from(pixel[2])
}

/// How far a grading moved a pixel, which is how a test can see which wheel
/// reached it: a tint that darkens and a tint that brightens both show up here.
fn reach(grading: Grading, rgb: [f32; 3]) -> f32 {
    let graded = super::process::grade(rgb, &grading);
    (0..3)
        .map(|channel| (graded[channel] - rgb[channel]).abs())
        .sum()
}

#[test]
fn the_grading_wheels_own_the_tones_they_are_named_for() {
    let wheel = |hue: f32| GradeWheel {
        hue,
        saturation: 100.0,
        luminance: 0.0,
    };
    let shadows = Grading {
        shadows: wheel(0.0),
        ..Grading::default()
    };
    let highlights = Grading {
        highlights: wheel(0.0),
        ..Grading::default()
    };
    // A shadow wheel reaches a dark pixel further than a bright one, and the
    // highlight wheel is the other way round.
    assert!(
        reach(shadows, [0.05, 0.05, 0.05]) > reach(highlights, [0.05, 0.05, 0.05]),
        "the shadow wheel owns the shadows"
    );
    assert!(
        reach(highlights, [0.9, 0.9, 0.9]) > reach(shadows, [0.9, 0.9, 0.9]),
        "the highlight wheel owns the highlights"
    );
    // Each wheel stops at the far end of the range rather than tinting the whole
    // picture, and they meet in the middle, which is what the blending is for.
    assert_eq!(
        reach(shadows, [0.9, 0.9, 0.9]),
        0.0,
        "the shadow wheel does not reach the highlights"
    );
    assert_eq!(
        reach(highlights, [0.05, 0.05, 0.05]),
        0.0,
        "nor the highlight wheel the shadows"
    );
    assert!(
        reach(shadows, [0.5, 0.5, 0.5]) > 0.0 && reach(highlights, [0.5, 0.5, 0.5]) > 0.0,
        "the wheels overlap through the midtones"
    );

    // The hue is the one that lands: a red wheel lifts red on a neutral pixel and
    // a blue wheel lifts blue.
    let red = reach(
        Grading {
            global: wheel(0.0),
            ..Grading::default()
        },
        [0.5, 0.5, 0.5],
    );
    let blue = reach(
        Grading {
            global: wheel(240.0),
            ..Grading::default()
        },
        [0.5, 0.5, 0.5],
    );
    assert!(
        red > 0.0 && blue > 0.0,
        "the global wheel reaches everything"
    );
    let graded_red = super::process::grade(
        [0.5, 0.5, 0.5],
        &Grading {
            global: wheel(0.0),
            ..Grading::default()
        },
    );
    let graded_blue = super::process::grade(
        [0.5, 0.5, 0.5],
        &Grading {
            global: wheel(240.0),
            ..Grading::default()
        },
    );
    assert!(
        graded_red[0] > graded_red[2] && graded_blue[2] > graded_blue[0],
        "each wheel tints with its own hue: {graded_red:?} and {graded_blue:?}"
    );

    // Balance moves the crossover, so a shadow wheel reaches further into the
    // picture when the balance favours shadows.
    let favouring_shadows = reach(
        Grading {
            shadows: wheel(0.0),
            balance: -100.0,
            ..Grading::default()
        },
        [0.5, 0.5, 0.5],
    );
    let favouring_highlights = reach(
        Grading {
            shadows: wheel(0.0),
            balance: 100.0,
            ..Grading::default()
        },
        [0.5, 0.5, 0.5],
    );
    assert!(
        favouring_shadows > favouring_highlights,
        "balance moves the crossover: {favouring_shadows} vs {favouring_highlights}"
    );

    // Blending widens how far a wheel reaches, so a pixel that sits in the
    // other wheel's territory still gets a little of it. The weights are
    // normalised, so this shows up as reach rather than as a stronger wheel.
    let narrow_highlight = reach(
        Grading {
            shadows: wheel(0.0),
            blending: 0.0,
            ..Grading::default()
        },
        [0.8, 0.8, 0.8],
    );
    let wide_highlight = reach(
        Grading {
            shadows: wheel(0.0),
            blending: 100.0,
            ..Grading::default()
        },
        [0.8, 0.8, 0.8],
    );
    assert!(
        wide_highlight > narrow_highlight,
        "a wider blend reaches further into the highlights: {wide_highlight} vs {narrow_highlight}"
    );
    let narrow_shadow = reach(
        Grading {
            highlights: wheel(0.0),
            blending: 0.0,
            ..Grading::default()
        },
        [0.2, 0.2, 0.2],
    );
    let wide_shadow = reach(
        Grading {
            highlights: wheel(0.0),
            blending: 100.0,
            ..Grading::default()
        },
        [0.2, 0.2, 0.2],
    );
    assert!(
        wide_shadow > narrow_shadow,
        "and into the shadows: {wide_shadow} vs {narrow_shadow}"
    );

    // A wheel with only a luminance shift moves brightness and no hue.
    let shifted = super::process::grade(
        [0.4, 0.4, 0.4],
        &Grading {
            global: GradeWheel {
                hue: 0.0,
                saturation: 0.0,
                luminance: 60.0,
            },
            ..Grading::default()
        },
    );
    let plain = super::process::grade([0.4, 0.4, 0.4], &Grading::default());
    assert!(
        shifted[0] > plain[0] && (shifted[0] - shifted[2]).abs() < 1e-5,
        "a luminance shift brightens without tinting: {shifted:?}"
    );
    let darkened = super::process::grade(
        [0.6, 0.6, 0.6],
        &Grading {
            global: GradeWheel {
                hue: 0.0,
                saturation: 0.0,
                luminance: -50.0,
            },
            ..Grading::default()
        },
    );
    assert!(
        darkened[0] < plain[0] + 0.2 && darkened[0] > 0.0,
        "a negative shift darkens: {darkened:?}"
    );
}

#[test]
fn a_grading_wheel_is_checked_before_it_is_trusted() {
    let wheel = GradeWheel {
        hue: 400.0,
        saturation: 10.0,
        luminance: 0.0,
    };
    let mut settings = DevelopSettings::default();
    settings.grading.global = wheel;
    assert!(settings.validate().is_err(), "a hue past 360 degrees");
    settings.grading.global.hue = f32::NAN;
    assert!(settings.validate().is_err(), "a hue that is not a number");
    settings.grading.global = GradeWheel::default();
    settings.grading.blending = 200.0;
    assert!(settings.validate().is_err(), "a blend past 100");
    settings.grading.blending = 50.0;
    settings.grading.balance = -200.0;
    assert!(settings.validate().is_err(), "a balance past the ends");
    settings.grading.balance = 0.0;
    settings.validate().unwrap();

    // A file written before the wheels existed reads back neutral.
    let mut older = serde_json::to_value(&settings).unwrap();
    assert!(older.get("grading").is_some(), "the wheels are stored");
    older.as_object_mut().unwrap().remove("grading");
    let without: DevelopSettings = serde_json::from_value(older).unwrap();
    assert!(!without.grading.adjusts());
    assert_eq!(
        without.grading.blending, 50.0,
        "the neutral blend is 50, not 0"
    );
}
