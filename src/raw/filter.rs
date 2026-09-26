//! The Camera Raw filter: the develop pipeline run over a layer that was
//! rendered already, rather than over a camera file.
//!
//! Compositor 1.2.3 added these controls as a filter as well as to Develop, so a
//! rendered layer can take the same tone, colour, detail, optics and geometry a
//! photograph does. Reusing the engine is possible because a `DecodedRaw` is
//! mectov's own struct rather than a camera's: an identity colour matrix and the
//! layer's own alpha are all that differ from what rawler produces.
//!
//! Two choices keep the default neutral, which is the contract Compositor's own
//! filter holds to. The colour matrices are the identity, so the trip through
//! linear light is a straight decode and encode of the same sRGB values, and
//! white balance is counted in relative steps rather than kelvin, because a
//! rendered image has no as-shot white balance to count from.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use image::{GrayImage, Luma, Rgb, Rgb32FImage, RgbaImage};

use super::settings::DevelopSettings;
use super::{DecodedRaw, RawMetadata, WhiteBalance, process};
use crate::color;

/// Share of a full warm swing the temperature slider applies to red and blue.
const TEMPERATURE_GAIN: f32 = 0.35;
/// Share of the tint swing that lands on red and blue.
const TINT_RED_BLUE: f32 = 0.15;
/// Share of the tint swing that pulls green down, which is the larger of the two.
const TINT_GREEN: f32 = 0.30;

/// The white balance gains a filter's relative sliders ask for.
///
/// Compositor counts temperature and tint from −100 to 100 rather than in kelvin,
/// so zero is exactly neutral: warm is positive, magenta is positive, and the
/// result is normalised back to a green of one so that a tint moves colour
/// without moving exposure.
pub fn relative_white_balance(temperature: f32, tint: f32) -> [f32; 3] {
    // A value the panel could not read, or one past the ends of its travel, is
    // the neutral one rather than a gain nobody asked for.
    fn step(value: f32) -> f32 {
        if value.is_finite() {
            (value / 100.0).clamp(-1.0, 1.0)
        } else {
            0.0
        }
    }
    let warm = step(temperature);
    let magenta = step(tint);
    let gains = [
        (1.0 + warm * TEMPERATURE_GAIN) * (1.0 + magenta * TINT_RED_BLUE),
        1.0 - magenta * TINT_GREEN,
        (1.0 - warm * TEMPERATURE_GAIN) * (1.0 + magenta * TINT_RED_BLUE),
    ];
    let green = gains[1].max(0.001);
    gains.map(|gain| (gain / green).clamp(0.01, 100.0))
}

/// The develop settings a filter's controls ask for.
///
/// A filter carries no crop, because a filter may not make a layer smaller, and
/// no as-shot or kelvin white balance, because a rendered image has neither: the
/// two relative sliders become custom gains instead, and the develop tint is
/// cleared so it cannot be applied a second time on top of them.
pub fn filter_settings(settings: &DevelopSettings, temperature: f32, tint: f32) -> DevelopSettings {
    let mut settings = settings.clone();
    settings.crop = [0.0, 0.0, 1.0, 1.0];
    settings.white_balance = WhiteBalance::Custom;
    settings.custom_wb = relative_white_balance(temperature, tint);
    settings.temperature = 6500.0;
    settings.tint = 0.0;
    settings
}

/// Turn a rendered layer into camera values the pipeline can develop.
///
/// sRGB is decoded to linear light, the colour matrices are the identity, and
/// the layer's alpha premultiplies the result, so that tone, clarity and
/// sharpen cannot pull colour out of a soft edge or make a halo where one was.
pub fn source(pixels: &RgbaImage) -> DecodedRaw {
    let (width, height) = pixels.dimensions();
    let mut camera = Rgb32FImage::new(width, height);
    let mut alpha = GrayImage::new(width, height);
    for (x, y, pixel) in pixels.enumerate_pixels() {
        let coverage = f32::from(pixel[3]) / 255.0;
        alpha.put_pixel(x, y, Luma([pixel[3]]));
        camera.put_pixel(
            x,
            y,
            Rgb(std::array::from_fn(|c| {
                color::decode_srgb(f32::from(pixel[c]) / 255.0) * coverage
            })),
        );
    }
    DecodedRaw {
        camera,
        alpha: Some(Arc::new(alpha)),
        as_shot: [1.0; 3],
        camera_to_rgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        xyz_to_camera: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        metadata: RawMetadata {
            width,
            height,
            ..Default::default()
        },
    }
}

/// Develop a rendered layer through the same pipeline a camera file takes.
///
/// The CPU path is used rather than the GPU one. A camera file has no alpha to
/// carry, and this filter's does, which the shader would need a fourth channel
/// for; the engine is shared with Develop, and a change to its shader for a
/// feature only this filter has is not worth the risk to the camera path.
pub fn render_filter(
    pixels: &RgbaImage,
    settings: &DevelopSettings,
    temperature: f32,
    tint: f32,
    cancel: &AtomicBool,
) -> Result<RgbaImage> {
    let source = source(pixels);
    let settings = filter_settings(settings, temperature, tint);
    process::render_cpu(&source, &settings, cancel)
}

/// A smaller copy of a rendered layer for the filter to preview itself with.
///
/// The layer's transform is not changed by this: the canvas draws whatever
/// buffer a layer holds at the size its transform says, so a proxy previews
/// correctly and Apply writes the pixels at full resolution.
pub fn preview_source(pixels: &RgbaImage, max_side: u32) -> RgbaImage {
    let scale = (max_side as f32 / pixels.width().max(pixels.height()) as f32).min(1.0);
    if scale >= 1.0 {
        return pixels.clone();
    }
    let size = (
        (pixels.width() as f32 * scale).round().max(1.0) as u32,
        (pixels.height() as f32 * scale).round().max(1.0) as u32,
    );
    image::imageops::resize(
        pixels,
        size.0,
        size.1,
        image::imageops::FilterType::Triangle,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_white_balance_slide_is_no_gain() {
        assert_eq!(relative_white_balance(0.0, 0.0), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn warmth_moves_red_and_blue_the_way_round() {
        let warm = relative_white_balance(100.0, 0.0);
        assert!(warm[0] > 1.0, "warmer red: {warm:?}");
        assert!(warm[2] < 1.0, "cooler blue: {warm:?}");
        let cool = relative_white_balance(-100.0, 0.0);
        assert!(cool[0] < 1.0 && cool[2] > 1.0, "{cool:?}");
    }

    #[test]
    fn tint_moves_colour_without_moving_exposure() {
        let magenta = relative_white_balance(0.0, 100.0);
        assert_eq!(magenta[1], 1.0, "green is the reference: {magenta:?}");
        assert!(magenta[0] > 1.0 && magenta[2] > 1.0, "{magenta:?}");
        let green = relative_white_balance(0.0, -100.0);
        assert_eq!(green[1], 1.0);
        assert!(green[0] < 1.0 && green[2] < 1.0, "{green:?}");
    }

    #[test]
    fn the_sliders_are_bounded_where_they_are_read() {
        // A value far outside the panel's range is clamped rather than trusted.
        assert_eq!(
            relative_white_balance(1_000.0, 0.0),
            relative_white_balance(100.0, 0.0)
        );
        assert_eq!(
            relative_white_balance(f32::NAN, 0.0),
            relative_white_balance(0.0, 0.0)
        );
    }

    #[test]
    fn a_filter_carries_neither_a_crop_nor_a_kelvin_white_balance() {
        let mut asked = DevelopSettings {
            crop: [0.25, 0.25, 0.75, 0.75],
            temperature: 4200.0,
            tint: 20.0,
            exposure: 0.5,
            ..Default::default()
        };
        asked.white_balance = WhiteBalance::AsShot;
        let settings = filter_settings(&asked, 0.0, 0.0);
        assert_eq!(settings.crop, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(settings.white_balance, WhiteBalance::Custom);
        assert_eq!(settings.tint, 0.0, "the develop tint would apply twice");
        assert_eq!(settings.exposure, 0.5, "the rest of the settings is kept");
        settings.validate().unwrap();
    }

    #[test]
    fn a_source_carries_the_layer_alpha_and_neutral_colour() {
        let pixels = RgbaImage::from_fn(4, 3, |x, _| image::Rgba([200, 100, 50, x as u8 * 60]));
        let source = source(&pixels);
        let alpha = source.alpha.as_ref().expect("a layer has alpha");
        assert_eq!(alpha.dimensions(), (4, 3));
        for (x, y, pixel) in pixels.enumerate_pixels() {
            assert_eq!(alpha.get_pixel(x, y)[0], pixel[3]);
            // The camera value is the premultiplied linear one.
            let expected =
                color::decode_srgb(f32::from(pixel[0]) / 255.0) * f32::from(pixel[3]) / 255.0;
            assert!((source.camera.get_pixel(x, y)[0] - expected).abs() < 1e-6);
        }
        assert_eq!(source.as_shot, [1.0; 3]);
        assert_eq!(source.metadata.width, 4);
        assert_eq!(source.metadata.height, 3);
    }

    #[test]
    fn a_preview_is_smaller_and_never_larger() {
        let pixels = RgbaImage::from_pixel(400, 300, image::Rgba([1, 2, 3, 255]));
        let proxy = preview_source(&pixels, 100);
        assert_eq!(proxy.dimensions(), (100, 75));
        assert_eq!(preview_source(&pixels, 500).dimensions(), (400, 300));
    }
}
