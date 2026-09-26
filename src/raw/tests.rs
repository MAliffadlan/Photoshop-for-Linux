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
