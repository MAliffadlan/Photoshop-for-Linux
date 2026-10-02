//! Compositor 1.3.3's Dither filter: a layer turned into dithered pixels, from
//! the classic Mac's Atkinson through Bayer grids, halftone screens, the old Mac
//! fill patterns and ASCII.
//!
//! The kernel is a straight port of Compositor's `DitherPixels.c`, including its
//! arithmetic: the error diffusion is serpentine and passes on only what its
//! kernel divides by, the tone is a gamma and a pivot, and the marks are decided
//! by a strict comparison against a shape's coverage. Nothing here is random, so
//! the same layer and the same settings give the same bytes every time.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::text::GlyphSet;

/// The dither styles, in the order Compositor's panel lists them. The numbers
/// are the ones its kernel switches on, so the two stay in step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DitherStyle {
    #[default]
    Atkinson,
    FloydSteinberg,
    Bayer2,
    Bayer4,
    Bayer8,
    HalftoneDots,
    HalftoneLines,
    HalftoneDiamonds,
    MacPatterns,
    Ascii,
}

/// One neighbour's share of the error, and the row and column it sits in.
struct Tap {
    dx: isize,
    dy: isize,
    weight: f32,
}

/// Atkinson passes on only six eighths of the error, which is what gives the
/// Mac's crisp, contrasty look. Floyd–Steinberg passes on all of it.
const ATKINSON: [Tap; 6] = [
    Tap {
        dx: 1,
        dy: 0,
        weight: 1.0,
    },
    Tap {
        dx: 2,
        dy: 0,
        weight: 1.0,
    },
    Tap {
        dx: -1,
        dy: 1,
        weight: 1.0,
    },
    Tap {
        dx: 0,
        dy: 1,
        weight: 1.0,
    },
    Tap {
        dx: 1,
        dy: 1,
        weight: 1.0,
    },
    Tap {
        dx: 0,
        dy: 2,
        weight: 1.0,
    },
];
const FLOYD: [Tap; 4] = [
    Tap {
        dx: 1,
        dy: 0,
        weight: 7.0,
    },
    Tap {
        dx: -1,
        dy: 1,
        weight: 3.0,
    },
    Tap {
        dx: 0,
        dy: 1,
        weight: 5.0,
    },
    Tap {
        dx: 1,
        dy: 1,
        weight: 1.0,
    },
];

/// The old Mac's fill patterns, eight rows of eight, the leftmost pixel in the
/// top bit, from sparsest to fullest.
const PATTERNS: [[u8; 8]; 17] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x80, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00],
    [0x88, 0x00, 0x22, 0x00, 0x88, 0x00, 0x22, 0x00],
    [0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01],
    [0x88, 0x22, 0x88, 0x22, 0x88, 0x22, 0x88, 0x22],
    [0x00, 0xFF, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00],
    [0x11, 0x22, 0x44, 0x88, 0x11, 0x22, 0x44, 0x88],
    [0xAA, 0x00, 0xAA, 0x00, 0xAA, 0x00, 0xAA, 0x00],
    [0x88, 0x55, 0x22, 0x55, 0x88, 0x55, 0x22, 0x55],
    [0xFF, 0x80, 0x80, 0x80, 0xFF, 0x08, 0x08, 0x08],
    [0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55],
    [0x81, 0x42, 0x24, 0x18, 0x18, 0x24, 0x42, 0x81],
    [0x77, 0xAA, 0xDD, 0xAA, 0x77, 0xAA, 0xDD, 0xAA],
    [0xEE, 0xDD, 0xBB, 0x77, 0xEE, 0xDD, 0xBB, 0x77],
    [0x77, 0xFF, 0xDD, 0xFF, 0x77, 0xFF, 0xDD, 0xFF],
    [0x7F, 0xFF, 0xFF, 0xFF, 0xF7, 0xFF, 0xFF, 0xFF],
    [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
];

/// The characters ASCII falls back to, in ascending ink.
pub const DEFAULT_CHARACTERS: &str = " .:-=+*#%@";

/// Every setting the filter has, with Compositor's own defaults and ranges. The
/// ranges are enforced when the filter runs, so a project or a preset cannot
/// hand the kernel a number it cannot use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DitherSettings {
    pub style: DitherStyle,
    /// How many layer pixels a dithered pixel covers on a side, for chunky
    /// old-screen pixels.
    pub pixel_size: f32,
    pub pixel_shape: PixelShape,
    /// Halftone screen cell size, in dithered pixels.
    pub cell_size: f32,
    /// ASCII's line height in layer pixels; the characters are about six tenths
    /// as wide.
    pub text_size: f32,
    /// The halftone screen's angle in degrees.
    pub angle: f32,
    /// Tones per channel for diffusion and ordered styles; two is 1-bit.
    pub levels: f32,
    /// How much of the error diffusion passes on, 0–100%. Less gives flatter,
    /// posterized areas.
    pub diffusion: f32,
    /// More ink (darker) or less, and flatter or punchier, before dithering.
    pub density: f32,
    pub contrast: f32,
    pub colors: DitherColors,
    pub dark: [f32; 3],
    pub light: [f32; 3],
    /// Marks stand for the light tones, drawn in the light colour on the dark:
    /// glowing dots on a black screen. Only the mark styles use it.
    #[serde(default = "default_true")]
    pub light_on_dark: bool,
    /// ASCII's characters, in any order: they are sorted by how much ink each
    /// one has.
    pub characters: String,
}

fn default_true() -> bool {
    true
}

impl Default for DitherSettings {
    fn default() -> Self {
        Self {
            style: DitherStyle::default(),
            pixel_size: 2.0,
            pixel_shape: PixelShape::default(),
            cell_size: 8.0,
            text_size: 14.0,
            angle: 45.0,
            levels: 2.0,
            diffusion: 100.0,
            density: 0.0,
            contrast: 0.0,
            colors: DitherColors::default(),
            dark: [0.0, 0.0, 0.0],
            light: [1.0, 1.0, 1.0],
            light_on_dark: true,
            characters: DEFAULT_CHARACTERS.into(),
        }
    }
}

/// How a chunky pixel is drawn: a solid square, or a round dot with the dark
/// colour showing around it, like the lit pixels of an LED screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelShape {
    #[default]
    Square,
    Dot,
}

/// Whether the result is made of the two picked colours or keeps the image's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DitherColors {
    #[default]
    BlackWhite,
    TwoColors,
    Original,
}

impl DitherStyle {
    pub const ALL: [DitherStyle; 10] = [
        DitherStyle::Atkinson,
        DitherStyle::FloydSteinberg,
        DitherStyle::Bayer2,
        DitherStyle::Bayer4,
        DitherStyle::Bayer8,
        DitherStyle::HalftoneDots,
        DitherStyle::HalftoneLines,
        DitherStyle::HalftoneDiamonds,
        DitherStyle::MacPatterns,
        DitherStyle::Ascii,
    ];

    /// The name Compositor's panel shows, which its menu reuses.
    pub fn name(self) -> &'static str {
        match self {
            DitherStyle::Atkinson => "Atkinson (Classic Mac)",
            DitherStyle::FloydSteinberg => "Floyd–Steinberg",
            DitherStyle::Bayer2 => "Bayer 2 × 2",
            DitherStyle::Bayer4 => "Bayer 4 × 4",
            DitherStyle::Bayer8 => "Bayer 8 × 8",
            DitherStyle::HalftoneDots => "Halftone Dots",
            DitherStyle::HalftoneLines => "Halftone Lines",
            DitherStyle::HalftoneDiamonds => "Halftone Diamonds",
            DitherStyle::MacPatterns => "Mac Patterns",
            DitherStyle::Ascii => "ASCII",
        }
    }

    /// The four groups Compositor's style list breaks its ten styles into, in
    /// its order. The last group carries no heading, as upstream's does not.
    pub const GROUPS: [(Option<&'static str>, &'static [DitherStyle]); 4] = [
        (
            Some("Error diffusion"),
            &[DitherStyle::Atkinson, DitherStyle::FloydSteinberg],
        ),
        (
            Some("Ordered"),
            &[
                DitherStyle::Bayer2,
                DitherStyle::Bayer4,
                DitherStyle::Bayer8,
            ],
        ),
        (
            Some("Halftone"),
            &[
                DitherStyle::HalftoneDots,
                DitherStyle::HalftoneLines,
                DitherStyle::HalftoneDiamonds,
            ],
        ),
        (None, &[DitherStyle::MacPatterns, DitherStyle::Ascii]),
    ];

    /// Error diffusion passes each pixel's rounding error to its neighbours.
    pub fn diffuses(self) -> bool {
        matches!(self, DitherStyle::Atkinson | DitherStyle::FloydSteinberg)
    }

    /// Diffusion and ordered styles quantize to a number of tones; the rest
    /// draw marks in two.
    pub fn has_tones(self) -> bool {
        self.diffuses()
            || matches!(
                self,
                DitherStyle::Bayer2 | DitherStyle::Bayer4 | DitherStyle::Bayer8
            )
    }

    pub fn is_halftone(self) -> bool {
        matches!(
            self,
            DitherStyle::HalftoneDots | DitherStyle::HalftoneLines | DitherStyle::HalftoneDiamonds
        )
    }

    /// Halftone shapes, patterns and characters mark one tone on the other, so
    /// which one is the mark matters.
    pub fn draws_marks(self) -> bool {
        !self.has_tones()
    }

    fn kernel(self) -> (&'static [Tap], f32) {
        match self {
            DitherStyle::Atkinson => (&ATKINSON, 8.0),
            _ => (&FLOYD, 16.0),
        }
    }

    /// The ordered threshold for a pixel, in 0…1. The smaller Bayer matrices are
    /// the top-left corners of the 8 × 8 one, rescaled, which is how the
    /// recursive construction nests them.
    fn ordered_threshold(self, x: usize, y: usize) -> f32 {
        const BAYER_2: [u8; 4] = [0, 2, 3, 1];
        const BAYER_4: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
        const BAYER_8: [u8; 64] = [
            0, 32, 8, 40, 2, 34, 10, 42, 48, 16, 56, 24, 50, 18, 58, 26, 12, 44, 4, 36, 14, 46, 6,
            38, 60, 28, 52, 20, 62, 30, 54, 22, 3, 35, 11, 43, 1, 33, 9, 41, 51, 19, 59, 27, 49,
            17, 57, 25, 15, 47, 7, 39, 13, 45, 5, 37, 63, 31, 55, 23, 61, 29, 53, 21,
        ];
        match self {
            DitherStyle::Bayer2 => (BAYER_2[(y & 1) * 2 + (x & 1)] as f32 + 0.5) / 4.0,
            DitherStyle::Bayer4 => (BAYER_4[(y & 3) * 4 + (x & 3)] as f32 + 0.5) / 16.0,
            _ => (BAYER_8[(y & 7) * 8 + (x & 7)] as f32 + 0.5) / 64.0,
        }
    }

    /// How much of a halftone cell a point must be covered by before it is
    /// marked, for each screen shape. `u` and `v` run from −0.5 to 0.5 across the
    /// cell, and the shapes grow from its middle as coverage rises.
    fn spot(self, u: f32, v: f32) -> f32 {
        match self {
            DitherStyle::HalftoneDots => std::f32::consts::PI * (u * u + v * v),
            DitherStyle::HalftoneLines => v.abs() * 2.0,
            _ => u.abs() + v.abs(),
        }
    }
}

impl PixelShape {
    pub const ALL: [PixelShape; 2] = [PixelShape::Square, PixelShape::Dot];

    pub fn name(self) -> &'static str {
        match self {
            PixelShape::Square => "Square",
            PixelShape::Dot => "Dot",
        }
    }
}

impl DitherColors {
    pub const ALL: [DitherColors; 3] = [
        DitherColors::BlackWhite,
        DitherColors::TwoColors,
        DitherColors::Original,
    ];

    pub fn name(self) -> &'static str {
        match self {
            DitherColors::BlackWhite => "Black & White",
            DitherColors::TwoColors => "Two Colors",
            DitherColors::Original => "Original",
        }
    }
}

impl DitherSettings {
    /// The three sizes counted in layer pixels, divided by how far the image that
    /// is about to be dithered is from full resolution.
    ///
    /// A chunky pixel, a halftone cell and a line of characters are distances in
    /// the real layer, so a preview handed a smaller copy has to ask for smaller
    /// numbers, or it would dither at the copy's scale and disagree with what
    /// Apply renders. The floors here are the copy's rather than the panel's: a
    /// size may be below the range a control offers and still be exactly the
    /// right number of the copy's pixels, which is the whole point of dividing.
    fn scaled_for(&self, scale: f32) -> DitherSettings {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let mut result = self.clone();
        result.pixel_size = (self.pixel_size / scale).max(1.0);
        result.cell_size = (self.cell_size / scale).max(2.0);
        result.text_size = (self.text_size / scale).max(1.0);
        result
    }

    /// Compositor clamps every control on the way to the kernel, rounding the
    /// ones that name a whole number of pixels or tones.
    pub fn normalized(&self) -> DitherSettings {
        let mut result = self.clone();
        result.pixel_size = clamp_round(self.pixel_size, 1.0, 32.0, 2.0);
        result.cell_size = clamp_round(self.cell_size, 4.0, 64.0, 8.0);
        result.text_size = clamp_round(self.text_size, 6.0, 64.0, 14.0);
        result.angle = clamp(self.angle, -90.0, 90.0, 45.0);
        result.levels = clamp_round(self.levels, 2.0, 8.0, 2.0);
        result.diffusion = clamp(self.diffusion, 0.0, 100.0, 100.0);
        result.density = clamp(self.density, -100.0, 100.0, 0.0);
        result.contrast = clamp(self.contrast, -100.0, 100.0, 0.0);
        result.dark = self.dark.map(|value| clamp(value, 0.0, 1.0, 0.0));
        result.light = self.light.map(|value| clamp(value, 0.0, 1.0, 1.0));
        result.characters = result
            .characters
            .chars()
            .filter(|character| !character.is_control())
            .take(64)
            .collect();
        result
    }
}

fn clamp(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

fn clamp_round(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    clamp(value, min, max, fallback).round()
}

/// Dither a layer. ASCII draws its characters at full resolution, so its pixel
/// size is ignored, as Compositor ignores it.
pub fn apply(image: &RgbaImage, settings: &DitherSettings) -> RgbaImage {
    apply_at_scale(image, settings, 1.0, &AtomicBool::new(false))
}

/// The same filter on an image that may be a smaller copy of the layer, `scale`
/// being the layer's full-resolution pixels per pixel of `image`. The sizes the
/// user picks are distances in layer pixels, so they are divided by the scale
/// before they are used; a cancelled pass hands the image back as it was given.
pub fn apply_at_scale(
    image: &RgbaImage,
    settings: &DitherSettings,
    scale: f32,
    cancel: &AtomicBool,
) -> RgbaImage {
    // The panel's own values first, clamped and rounded as it shows them, then
    // those distances read in the pixels of the image actually in hand.
    let settings = settings.normalized().scaled_for(scale);
    let glyphs = (settings.style == DitherStyle::Ascii)
        .then(|| monospace_glyphs(&settings))
        .flatten();
    let block = if settings.style == DitherStyle::Ascii {
        1
    } else {
        settings.pixel_size.round().max(1.0) as u32
    };
    let dithered = if block > 1 {
        // Chunky pixels: dither a copy averaged down by the pixel size, then
        // blow it back up without smoothing.
        let small = block_average(image, block, cancel);
        let dithered = dither_pixels(&small, &settings, glyphs.as_ref(), cancel);
        block_nearest(&dithered, block, image.width(), image.height())
    } else {
        dither_pixels(image, &settings, glyphs.as_ref(), cancel)
    };
    if cancelled(cancel) {
        return image.clone();
    }
    if settings.pixel_shape == PixelShape::Dot && block > 1 {
        let rounded = round_the_dots(&dithered, block, &settings, cancel);
        return if cancelled(cancel) {
            image.clone()
        } else {
            rounded
        };
    }
    dithered
}

/// Whether the pass has been called off. Every scanline asks, which is a load
/// against a row of pixel work, so a job the user has moved past stops within a
/// row rather than running to the end of a large layer.
fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

/// The bytes the kernel's own working set takes for each pixel of the image it
/// is given: the image's colours, which only `Original` keeps, the alpha, the
/// result, and a tone plane per channel. The mark styles hold their marks beside
/// the tone as well, so `Original` is measured with that second plane even where
/// a tone style would not make one.
///
/// This is the number the caller measures a layer against the machine's memory
/// with, so it is stated once, here.
pub const fn bytes_per_pixel(colors: DitherColors) -> u64 {
    const SOURCE: u64 = 12;
    const ALPHA: u64 = 1;
    const OUT: u64 = 4;
    const TONE: u64 = 4;
    const MARKS: u64 = 4;
    match colors {
        DitherColors::Original => SOURCE + ALPHA + OUT + TONE * 3 + MARKS,
        _ => ALPHA + OUT + TONE,
    }
}

/// The bytes one dither of `pixels` pixels takes, which is what a caller compares
/// against the memory a machine may spare before asking for the allocation.
pub fn working_set(pixels: u64, colors: DitherColors) -> u64 {
    pixels.saturating_mul(bytes_per_pixel(colors))
}

/// The system fonts, read once. Compositor draws its ASCII from the system
/// monospaced face, so the glyphs depend on the machine the way its own do.
fn monospace_glyphs(settings: &DitherSettings) -> Option<GlyphSet> {
    // The font system is read once and shared: loading every installed family
    // costs more than the filter itself.
    static FONTS: LazyLock<std::sync::Mutex<crate::text::TextRenderer>> =
        LazyLock::new(|| std::sync::Mutex::new(Default::default()));
    let characters = if settings.characters.is_empty() {
        DEFAULT_CHARACTERS
    } else {
        settings.characters.as_str()
    };
    let mut fonts = FONTS.lock().ok()?;
    crate::text::monospace_glyphs(
        &mut fonts,
        characters,
        settings.text_size.round().max(1.0) as u32,
    )
}

/// The average of each block of pixels, which is what a chunky pixel stands for.
/// A partial block at the right or bottom edge averages only the pixels there.
fn block_average(image: &RgbaImage, block: u32, cancel: &AtomicBool) -> RgbaImage {
    let (width, height) = image.dimensions();
    let small_width = width.div_ceil(block);
    let small_height = height.div_ceil(block);
    let mut out = RgbaImage::new(small_width, small_height);
    for sy in 0..small_height {
        if cancelled(cancel) {
            return out;
        }
        for sx in 0..small_width {
            let mut total = [0.0_f32; 4];
            let mut count = 0.0_f32;
            for y in (sy * block)..((sy + 1) * block).min(height) {
                for x in (sx * block)..((sx + 1) * block).min(width) {
                    let pixel = image.get_pixel(x, y).0;
                    for c in 0..4 {
                        total[c] += f32::from(pixel[c]);
                    }
                    count += 1.0;
                }
            }
            out.put_pixel(
                sx,
                sy,
                Rgba(std::array::from_fn(|c| {
                    (total[c] / count.max(1.0)).round() as u8
                })),
            );
        }
    }
    out
}

/// Each dithered pixel painted back over the block it came from, unsmoothed. A
/// partial block at the right or bottom edge is left showing whatever the last
/// dithered pixel was, which is what a nearest-neighbour blow-up does.
fn block_nearest(image: &RgbaImage, block: u32, width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| *image.get_pixel(x / block, y / block))
}

fn dither_pixels(
    image: &RgbaImage,
    settings: &DitherSettings,
    glyphs: Option<&GlyphSet>,
    cancel: &AtomicBool,
) -> RgbaImage {
    let (width, height) = image.dimensions();
    // Counted in u64 first so that a layer wide enough to overflow the 32-bit
    // product the kernel used to take cannot wrap into a shorter pass.
    let count = (u64::from(width) * u64::from(height)) as usize;
    if count == 0 {
        return image.clone();
    }
    let original = settings.colors == DitherColors::Original;
    let planes = if original { 3 } else { 1 };
    // The image's own colours, unadjusted: the mark styles take them in
    // Original mode, and no other mode reads them at all, so the planes are only
    // kept where they are used.
    let mut source = if original {
        vec![[0.0_f32; 3]; count]
    } else {
        Vec::new()
    };
    let mut alpha = vec![0_u8; count];
    let gamma = 2.0_f32.powf(settings.density / 100.0 * 1.5);
    let contrast = if settings.contrast >= 0.0 {
        1.0 / (1.0 - 0.95 * settings.contrast / 100.0)
    } else {
        1.0 + settings.contrast / 100.0
    };
    let mut tone = vec![0.0_f32; count * planes];
    for y in 0..height {
        if cancelled(cancel) {
            return image.clone();
        }
        for x in 0..width {
            let at = (y * width + x) as usize;
            let pixel = image.get_pixel(x, y).0;
            alpha[at] = pixel[3];
            let (r, g, b) = if pixel[3] == 0 {
                (0.0, 0.0, 0.0)
            } else {
                let scale = 1.0 / f32::from(pixel[3]);
                (
                    f32::from(pixel[0]) * scale,
                    f32::from(pixel[1]) * scale,
                    f32::from(pixel[2]) * scale,
                )
            };
            if original {
                tone[at] = adjust_tone(r, gamma, contrast);
                tone[count + at] = adjust_tone(g, gamma, contrast);
                tone[2 * count + at] = adjust_tone(b, gamma, contrast);
                source[at] = [r, g, b];
            } else {
                tone[at] = adjust_tone(0.2126 * r + 0.7152 * g + 0.0722 * b, gamma, contrast);
            }
        }
    }

    let dark = settings.dark;
    let light = settings.light;
    let mut out = image.clone();
    let style = settings.style;
    let levels = (settings.levels as i32).clamp(2, 16);
    if style.has_tones() {
        if style.diffuses() {
            for plane in 0..planes {
                let offset = plane * count;
                diffuse(
                    &mut tone[offset..offset + count],
                    &alpha,
                    width as usize,
                    height as usize,
                    style,
                    levels,
                    settings.diffusion / 100.0,
                    cancel,
                );
                if cancelled(cancel) {
                    return image.clone();
                }
            }
        } else {
            for plane in 0..planes {
                let offset = plane * count;
                for y in 0..height as usize {
                    if cancelled(cancel) {
                        return image.clone();
                    }
                    for x in 0..width as usize {
                        let at = y * width as usize + x;
                        if alpha[at] == 0 {
                            continue;
                        }
                        tone[offset + at] =
                            ordered(tone[offset + at], style.ordered_threshold(x, y), levels);
                    }
                }
            }
        }
        for y in 0..height {
            if cancelled(cancel) {
                return image.clone();
            }
            for x in 0..width {
                let at = (y * width + x) as usize;
                if alpha[at] == 0 {
                    continue;
                }
                let colour = if original {
                    [tone[at], tone[count + at], tone[2 * count + at]]
                } else {
                    let t = tone[at];
                    std::array::from_fn(|c| dark[c] + (light[c] - dark[c]) * t)
                };
                write_pixel(out.get_pixel_mut(x, y), colour);
            }
        }
        return out;
    }

    // Marks cover as much of each spot as the tone calls for. On light they
    // stand for darkness and are drawn in the dark colour; light on dark, the
    // reverse.
    let marks = if original {
        (0..count)
            .map(|at| 0.2126 * tone[at] + 0.7152 * tone[count + at] + 0.0722 * tone[2 * count + at])
            .collect()
    } else {
        tone
    };
    let cell = (settings.cell_size as i32).max(2) as f32;
    let angle = settings.angle.to_radians();
    let (sin, cos) = angle.sin_cos();
    let (ink, paper) = if settings.light_on_dark {
        (light, dark)
    } else {
        (dark, light)
    };
    // Glyphs: one cell each, picked from the cell's average tone and worked out
    // once per cell.
    let glyphs = glyphs.filter(|set| !set.coverage.is_empty());
    let (columns, rows) = glyphs.map_or((0, 0), |set| {
        let cell_width = set.width.max(1);
        let cell_height = set.height.max(1);
        (
            (width as usize).div_ceil(cell_width),
            (height as usize).div_ceil(cell_height),
        )
    });
    let mut picked = vec![0_usize; columns * rows];
    if let Some(set) = glyphs {
        for row in 0..rows {
            if cancelled(cancel) {
                return image.clone();
            }
            for column in 0..columns {
                let mut sum = 0.0_f32;
                let mut n = 0_u32;
                for y in (row * set.height)..((row + 1) * set.height).min(height as usize) {
                    for x in (column * set.width)..((column + 1) * set.width).min(width as usize) {
                        let at = y * width as usize + x;
                        if alpha[at] != 0 {
                            sum += marks[at];
                            n += 1;
                        }
                    }
                }
                let t = if n > 0 { sum / n as f32 } else { 1.0 };
                let wanted = (if settings.light_on_dark { t } else { 1.0 - t })
                    * set.coverage[set.coverage.len() - 1];
                let mut best = 0;
                let mut best_distance = 2.0_f32;
                for (index, coverage) in set.coverage.iter().enumerate() {
                    let distance = (coverage - wanted).abs();
                    if distance < best_distance {
                        best_distance = distance;
                        best = index;
                    }
                }
                picked[row * columns + column] = best;
            }
        }
    }
    // In Original mode the marks take the pixel's own colour, over black (light
    // on dark) or white.
    let original_paper = if settings.light_on_dark { 0.0 } else { 1.0 };
    for y in 0..height as usize {
        if cancelled(cancel) {
            return image.clone();
        }
        for x in 0..width as usize {
            let at = y * width as usize + x;
            if alpha[at] == 0 {
                continue;
            }
            let t = marks[at];
            let coverage = if settings.light_on_dark { t } else { 1.0 - t };
            let amount = if let Some(set) = glyphs {
                let glyph = picked[(y / set.height) * columns + x / set.width];
                f32::from(
                    set.maps[glyph * set.width * set.height
                        + (y % set.height) * set.width
                        + x % set.width],
                ) / 255.0
            } else if style == DitherStyle::MacPatterns {
                let index = (coverage * (PATTERNS.len() - 1) as f32).round() as usize;
                f32::from((PATTERNS[index][y & 7] >> (7 - (x & 7))) & 1)
            } else {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let mut u = (fx * cos + fy * sin) / cell;
                let mut v = (-fx * sin + fy * cos) / cell;
                u -= u.floor() + 0.5;
                v -= v.floor() + 0.5;
                f32::from(coverage > style.spot(u, v))
            };
            let colour = if original {
                std::array::from_fn(|c| original_paper + (source[at][c] - original_paper) * amount)
            } else {
                std::array::from_fn(|c| paper[c] + (ink[c] - paper[c]) * amount)
            };
            write_pixel(out.get_pixel_mut(x as u32, y as u32), colour);
        }
    }
    out
}

/// Density darkens or lightens as a gamma, so black and white stay put; contrast
/// pivots on mid grey.
fn adjust_tone(value: f32, gamma: f32, contrast: f32) -> f32 {
    let value = value.clamp(0.0, 1.0).powf(gamma);
    ((value - 0.5) * contrast + 0.5).clamp(0.0, 1.0)
}

fn quantize(value: f32, levels: i32) -> f32 {
    let steps = (levels - 1) as f32;
    (value.clamp(0.0, 1.0) * steps).round() / steps
}

/// Diffuses one plane in serpentine order, so the error's drift doesn't streak to
/// one side. The taps are mirrored on the rows that run backwards, so the error
/// only ever lands on pixels the pass has not reached.
#[allow(clippy::too_many_arguments)]
fn diffuse(
    plane: &mut [f32],
    alpha: &[u8],
    width: usize,
    height: usize,
    style: DitherStyle,
    levels: i32,
    diffusion: f32,
    cancel: &AtomicBool,
) {
    let (taps, divisor) = style.kernel();
    for y in 0..height {
        if cancelled(cancel) {
            return;
        }
        let reverse = y & 1 == 1;
        for i in 0..width {
            let x = if reverse { width - 1 - i } else { i };
            let at = y * width + x;
            if alpha[at] == 0 {
                continue;
            }
            let old = plane[at];
            let quantized = quantize(old, levels);
            plane[at] = quantized;
            let error = (old - quantized) * diffusion / divisor;
            for tap in taps {
                let nx = x as isize + if reverse { -tap.dx } else { tap.dx };
                let ny = y as isize + tap.dy;
                if nx < 0 || nx >= width as isize || ny < 0 || ny >= height as isize {
                    continue;
                }
                plane[ny as usize * width + nx as usize] += error * tap.weight;
            }
        }
    }
}

fn ordered(value: f32, threshold: f32, levels: i32) -> f32 {
    let steps = (levels - 1) as f32;
    let quantized = (value.clamp(0.0, 1.0) * steps + threshold)
        .floor()
        .min(steps);
    quantized / steps
}

/// The result is premultiplied, and the alpha it already has is never touched.
fn write_pixel(pixel: &mut image::Rgba<u8>, colour: [f32; 3]) {
    let alpha = f32::from(pixel.0[3]) / 255.0;
    // The three channels and no further, so the alpha is left as it was.
    for (channel, colour) in pixel.0.iter_mut().zip(colour) {
        *channel = (colour.clamp(0.0, 1.0) * alpha * 255.0).round() as u8;
    }
}

/// The Dot shape: each chunky pixel eroded toward a circle, with the dark colour
/// showing through the gap around it.
fn round_the_dots(
    image: &RgbaImage,
    block: u32,
    settings: &DitherSettings,
    cancel: &AtomicBool,
) -> RgbaImage {
    if block < 2 {
        return image.clone();
    }
    let gap = if settings.colors == DitherColors::TwoColors {
        settings.dark
    } else {
        [0.0, 0.0, 0.0]
    };
    let radius = block as f32 * 0.42;
    let middle = block as f32 / 2.0;
    let (width, height) = image.dimensions();
    let mut out = image.clone();
    for y in 0..height {
        if cancelled(cancel) {
            return out;
        }
        let dy = (y % block) as f32 + 0.5 - middle;
        for x in 0..width {
            let pixel = image.get_pixel(x, y).0;
            if pixel[3] == 0 {
                continue;
            }
            let dx = (x % block) as f32 + 0.5 - middle;
            let cover = (radius - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            if cover >= 1.0 {
                continue;
            }
            let mut rounded = [0_u8; 4];
            for c in 0..3 {
                rounded[c] = (f32::from(pixel[c]) * cover
                    + gap[c] * (f32::from(pixel[3]) / 255.0) * (1.0 - cover))
                    .round() as u8;
            }
            rounded[3] = pixel[3];
            out.put_pixel(x, y, image::Rgba(rounded));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// A horizontal ramp across the frame, which every style has an opinion
    /// about: dark on the left, light on the right, with a transparent column at
    /// the very left so the alpha handling is part of every test.
    fn ramp() -> RgbaImage {
        RgbaImage::from_fn(24, 8, |x, _y| {
            if x == 0 {
                Rgba([0, 0, 0, 0])
            } else {
                let level = (x * 11).min(255) as u8;
                Rgba([level, level / 2, 255 - level, 255])
            }
        })
    }

    fn quiet() -> DitherSettings {
        DitherSettings {
            pixel_size: 1.0,
            ..Default::default()
        }
    }

    #[test]
    fn the_defaults_are_compositors_own() {
        let settings = DitherSettings::default();
        assert_eq!(settings.style, DitherStyle::Atkinson);
        assert_eq!(settings.pixel_size, 2.0);
        assert_eq!(settings.cell_size, 8.0);
        assert_eq!(settings.text_size, 14.0);
        assert_eq!(settings.angle, 45.0);
        assert_eq!(settings.levels, 2.0);
        assert_eq!(settings.diffusion, 100.0);
        assert_eq!(settings.density, 0.0);
        assert_eq!(settings.contrast, 0.0);
        assert_eq!(settings.colors, DitherColors::BlackWhite);
        assert_eq!(settings.dark, [0.0, 0.0, 0.0]);
        assert_eq!(settings.light, [1.0, 1.0, 1.0]);
        assert!(settings.light_on_dark);
        assert_eq!(settings.characters, " .:-=+*#%@");
        // The names are the panel's, down to the dashes and multiplication signs.
        assert_eq!(DitherStyle::FloydSteinberg.name(), "Floyd–Steinberg");
        assert_eq!(DitherStyle::Bayer2.name(), "Bayer 2 × 2");
        assert_eq!(DitherStyle::Atkinson.name(), "Atkinson (Classic Mac)");
        // And the styles divide the way the panel's menu does.
        assert!(DitherStyle::Atkinson.diffuses());
        assert!(DitherStyle::FloydSteinberg.diffuses());
        assert!(!DitherStyle::Bayer4.diffuses());
        assert!(DitherStyle::Bayer8.has_tones());
        assert!(!DitherStyle::HalftoneDots.has_tones());
        assert!(DitherStyle::HalftoneLines.is_halftone());
        assert!(!DitherStyle::MacPatterns.is_halftone());
        assert!(DitherStyle::Ascii.draws_marks());
    }

    #[test]
    fn every_style_keeps_the_layers_shape_and_its_alpha() {
        let pixels = ramp();
        for style in DitherStyle::ALL {
            let result = apply(&pixels, &DitherSettings { style, ..quiet() });
            assert_eq!(
                result.dimensions(),
                pixels.dimensions(),
                "{style:?} keeps the layer's size"
            );
            assert_eq!(
                result.get_pixel(0, 0).0,
                [0, 0, 0, 0],
                "{style:?} leaves transparency"
            );
            for x in 0..pixels.width() {
                for y in 0..pixels.height() {
                    assert_eq!(
                        result.get_pixel(x, y).0[3],
                        pixels.get_pixel(x, y).0[3],
                        "{style:?} keeps the alpha at {x},{y}"
                    );
                }
            }
            // Deterministic: the same layer and settings give the same bytes.
            assert_eq!(
                result,
                apply(&pixels, &DitherSettings { style, ..quiet() }),
                "{style:?} is not random"
            );
        }
    }

    #[test]
    fn one_bit_diffusion_reduces_a_ramp_to_two_ends() {
        let pixels = ramp();
        let result = apply(&pixels, &quiet());
        let shades: Vec<u8> = (1..pixels.width())
            .map(|x| result.get_pixel(x, 0).0[0])
            .collect();
        assert!(
            shades.iter().all(|shade| *shade < 8 || *shade > 247),
            "two tones, not a ramp: {shades:?}"
        );
        assert!(
            shades.windows(2).any(|pair| pair[0] < 8 && pair[1] > 247),
            "and the dithered pattern is a mix of both, not a hard edge"
        );
        // The dark end is black and the light end white, because the colours are
        // black and white.
        assert!(
            shades
                .iter()
                .all(|shade| *shade == shades[0] || *shade == 255)
        );
    }

    #[test]
    fn more_tones_and_more_diffusion_change_the_pattern() {
        let pixels = ramp();
        let shades = |settings: DitherSettings| -> Vec<u8> {
            apply(&pixels, &settings)
                .pixels()
                .map(|pixel| pixel.0[0])
                .collect()
        };
        let one_bit = shades(quiet());
        let four_tones = shades(DitherSettings {
            levels: 4.0,
            ..quiet()
        });
        assert_ne!(one_bit, four_tones, "levels is a quantizer");
        assert!(
            four_tones.iter().any(|shade| *shade > 8 && *shade < 247),
            "four tones means a middle tone appears"
        );
        let flat = shades(DitherSettings {
            diffusion: 0.0,
            ..quiet()
        });
        assert_ne!(one_bit, flat, "no diffusion is a harder threshold");
    }

    #[test]
    fn density_darkens_and_contrast_pivots_on_mid_grey() {
        let pixels = RgbaImage::from_pixel(16, 4, Rgba([200, 200, 200, 255]));
        let plain = apply(&pixels, &quiet());
        let darker = apply(
            &pixels,
            &DitherSettings {
                density: 100.0,
                levels: 8.0,
                ..quiet()
            },
        );
        let lighter = apply(
            &pixels,
            &DitherSettings {
                density: -100.0,
                levels: 8.0,
                ..quiet()
            },
        );
        assert_eq!(plain.dimensions(), darker.dimensions());
        assert_ne!(plain, darker, "more ink is a different picture");
        assert_ne!(plain, lighter);
        // Black and white are fixed points of the gamma, so they stay put.
        let extremes = RgbaImage::from_fn(8, 2, |x, _| {
            if x < 4 {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([255, 255, 255, 255])
            }
        });
        let kept = apply(
            &extremes,
            &DitherSettings {
                density: 100.0,
                contrast: 50.0,
                ..quiet()
            },
        );
        assert!(
            kept.get_pixel(0, 0).0[0] < 128 && kept.get_pixel(7, 0).0[0] > 128,
            "the ends survive: {:?}",
            kept.get_pixel(0, 0).0
        );
    }

    #[test]
    fn each_bayer_matrix_is_the_one_compositor_ships() {
        // The thresholds are Compositor's numbers: each matrix's value plus half a
        // step, over the number of cells.
        assert!((DitherStyle::Bayer2.ordered_threshold(0, 0) - 0.125).abs() < 1e-6);
        assert!((DitherStyle::Bayer2.ordered_threshold(1, 0) - 0.625).abs() < 1e-6);
        assert!((DitherStyle::Bayer2.ordered_threshold(0, 1) - 0.875).abs() < 1e-6);
        assert!((DitherStyle::Bayer2.ordered_threshold(1, 1) - 0.375).abs() < 1e-6);
        assert!((DitherStyle::Bayer4.ordered_threshold(0, 0) - 0.5 / 16.0).abs() < 1e-6);
        assert!((DitherStyle::Bayer4.ordered_threshold(1, 0) - 8.5 / 16.0).abs() < 1e-6);
        assert!((DitherStyle::Bayer8.ordered_threshold(0, 0) - 0.5 / 64.0).abs() < 1e-6);
        assert!((DitherStyle::Bayer8.ordered_threshold(1, 0) - 32.5 / 64.0).abs() < 1e-6);
        // Each matrix tiles with its own period.
        for style in [
            DitherStyle::Bayer2,
            DitherStyle::Bayer4,
            DitherStyle::Bayer8,
        ] {
            assert_eq!(
                style.ordered_threshold(0, 0),
                style.ordered_threshold(8, 8),
                "{style:?} repeats every eight"
            );
        }
        // Each one differs from its neighbour inside its own period, which is
        // what makes it a pattern rather than a constant.
        for style in [
            DitherStyle::Bayer2,
            DitherStyle::Bayer4,
            DitherStyle::Bayer8,
        ] {
            assert_ne!(
                style.ordered_threshold(0, 0),
                style.ordered_threshold(1, 0),
                "{style:?} varies across its row"
            );
            assert_ne!(
                style.ordered_threshold(0, 0),
                style.ordered_threshold(0, 1),
                "{style:?} and down its column"
            );
        }
        assert_ne!(
            DitherStyle::Bayer4.ordered_threshold(1, 0),
            DitherStyle::Bayer8.ordered_threshold(1, 0),
            "and the 4 × 4 is not a rescaled 8 × 8 at the same cell"
        );

        // Each matrix is a permutation of its own uniform thresholds, which is
        // what makes it a dither rather than a gradient.
        let grid = |style: DitherStyle, n: usize| -> Vec<f32> {
            (0..n * n)
                .map(|index| style.ordered_threshold(index % n, index / n))
                .collect()
        };
        for (style, n) in [
            (DitherStyle::Bayer2, 2_usize),
            (DitherStyle::Bayer4, 4),
            (DitherStyle::Bayer8, 8),
        ] {
            let mut sorted = grid(style, n);
            sorted.sort_by(f32::total_cmp);
            let every: Vec<f32> = (0..n * n)
                .map(|index| (index as f32 + 0.5) / (n * n) as f32)
                .collect();
            assert_eq!(sorted, every, "{style:?} uses every threshold once");
        }
        assert_ne!(
            grid(DitherStyle::Bayer2, 2),
            grid(DitherStyle::Bayer4, 4),
            "and the 2 × 2 is not laid out like the 4 × 4"
        );
        assert_ne!(
            grid(DitherStyle::Bayer4, 4),
            grid(DitherStyle::Bayer8, 8),
            "nor the 4 × 4 like the 8 × 8"
        );

        // A ramp is where the three part company on the picture as well. One flat
        // tone will not do: it quantizes the same way through whichever matrix
        // happens to straddle that tone, which says more about the tone than about
        // the matrix.
        let flat = RgbaImage::from_fn(16, 16, |x, y| {
            let level = (x * 9 + y * 5 + 7) as u8;
            Rgba([level, level, level, 255])
        });
        let pattern = |style| -> Vec<u8> {
            apply(
                &flat,
                &DitherSettings {
                    style,
                    levels: 8.0,
                    ..quiet()
                },
            )
            .pixels()
            .map(|pixel| pixel.0[0])
            .collect()
        };
        let two = pattern(DitherStyle::Bayer2);
        let four = pattern(DitherStyle::Bayer4);
        let eight = pattern(DitherStyle::Bayer8);
        assert_ne!(two, four, "the 4 × 4 does not quantize like the 2 × 2");
        assert_ne!(four, eight, "nor the 8 × 8 like the 4 × 4");
        assert_ne!(two, eight);
        // The scale's own tones, which land on a byte each once the tone is
        // written out and rounded.
        let tones: Vec<u8> = (0..=7_u32)
            .map(|step| (step as f32 * 255.0 / 7.0).round() as u8)
            .collect();
        assert!(
            four.iter().all(|shade| tones.contains(shade)),
            "an ordered dither snaps to the scale's tones and no further, {tones:?}"
        );
    }

    #[test]
    fn the_halftone_screens_draw_three_different_shapes() {
        // The shapes Compositor grows from the middle of a cell: a dot is a
        // circle's worth of coverage, lines run across the cell whatever the
        // other axis holds, and a diamond is the sum of the two distances.
        let close = |a: f32, b: f32| (a - b).abs() < 1e-6;
        assert!(close(DitherStyle::HalftoneDots.spot(0.0, 0.0), 0.0));
        assert!(close(
            DitherStyle::HalftoneDots.spot(0.25, 0.0),
            std::f32::consts::PI * 0.0625
        ));
        assert!(close(
            DitherStyle::HalftoneDots.spot(0.3, 0.4),
            std::f32::consts::PI * 0.25
        ));
        assert!(
            close(DitherStyle::HalftoneLines.spot(0.3, 0.0), 0.0),
            "the middle of a cell's line is inked at any tone"
        );
        assert!(close(DitherStyle::HalftoneLines.spot(0.3, 0.25), 0.5));
        assert!(
            close(
                DitherStyle::HalftoneLines.spot(0.3, 0.1),
                DitherStyle::HalftoneLines.spot(0.45, 0.1)
            ),
            "a line does not care how far along the cell it is"
        );
        assert!(
            !close(
                DitherStyle::HalftoneDiamonds.spot(0.3, 0.1),
                DitherStyle::HalftoneDiamonds.spot(0.45, 0.1)
            ),
            "a diamond does"
        );
        assert!(close(DitherStyle::HalftoneDiamonds.spot(0.3, 0.1), 0.4));
        assert!(close(DitherStyle::HalftoneDiamonds.spot(0.1, 0.1), 0.2));

        // And on the picture they draw three different screens: a mid tone marks
        // about half of every cell whichever shape it is, so the picture differs
        // in where the marks sit rather than how many there are.
        let pixels = RgbaImage::from_pixel(24, 24, Rgba([128, 128, 128, 255]));
        let screen = |style| -> RgbaImage {
            apply(
                &pixels,
                &DitherSettings {
                    style,
                    cell_size: 8.0,
                    angle: 0.0,
                    light_on_dark: false,
                    ..quiet()
                },
            )
        };
        let dots = screen(DitherStyle::HalftoneDots);
        let lines = screen(DitherStyle::HalftoneLines);
        let diamonds = screen(DitherStyle::HalftoneDiamonds);
        assert_ne!(dots, lines);
        assert_ne!(lines, diamonds);
        assert_ne!(dots, diamonds);
        // A line inks a whole row of the cell or none of it, so every row of its
        // screen is either paper or entirely marked. A circle is narrower away
        // from a cell's middle, so it leaves rows marked in part.
        let lit = |image: &RgbaImage, y: u32| -> usize {
            (0..24)
                .filter(|x| image.get_pixel(*x, y).0[0] > 127)
                .count()
        };
        let partly = |image: &RgbaImage| {
            (0..24).any(|y| {
                let marked = lit(image, y);
                marked > 0 && marked < 24
            })
        };
        assert!(
            partly(&dots),
            "a circle is only as wide as its cell's middle"
        );
        assert!(!partly(&lines), "a line is as wide as the cell");
        // Ink and paper swap when the marks stand for the light tones instead.
        assert_ne!(
            screen(DitherStyle::HalftoneDots),
            apply(
                &pixels,
                &DitherSettings {
                    style: DitherStyle::HalftoneDots,
                    cell_size: 8.0,
                    angle: 0.0,
                    light_on_dark: true,
                    ..quiet()
                },
            ),
            "light on dark is the other way round"
        );
        // The angle turns the screen.
        assert_ne!(
            screen(DitherStyle::HalftoneLines),
            apply(
                &pixels,
                &DitherSettings {
                    style: DitherStyle::HalftoneLines,
                    cell_size: 8.0,
                    angle: 45.0,
                    light_on_dark: false,
                    ..quiet()
                },
            ),
            "45 degrees is not 0"
        );
    }

    #[test]
    fn mac_patterns_offer_seventeen_steps() {
        let pixels = RgbaImage::from_fn(8, 8, |x, y| {
            let level = (x + y) * 16;
            Rgba([level.min(255) as u8; 4])
        });
        let dithered = apply(
            &pixels,
            &DitherSettings {
                style: DitherStyle::MacPatterns,
                ..quiet()
            },
        );
        let ink: usize = dithered.pixels().filter(|pixel| pixel.0[0] > 127).count();
        assert!(ink > 0 && ink < 64, "a sparse pattern, not a flood: {ink}");
        // The tile repeats: the pattern is eight pixels wide and tall, so the
        // first and ninth columns of a wider layer agree.
        let wide = apply(
            &RgbaImage::from_fn(16, 8, |x, y| {
                let level = ((x % 8) + y) * 16;
                Rgba([level.min(255) as u8; 4])
            }),
            &DitherSettings {
                style: DitherStyle::MacPatterns,
                ..quiet()
            },
        );
        for y in 0..8 {
            assert_eq!(
                wide.get_pixel(0, y).0,
                wide.get_pixel(8, y).0,
                "the pattern tiles every eight pixels at row {y}"
            );
        }
    }

    #[test]
    fn the_two_colours_are_lerped_and_the_originals_are_kept() {
        let pixels = RgbaImage::from_fn(8, 4, |x, _| {
            let level = (x * 36).min(255) as u8;
            Rgba([level, level, level, 255])
        });
        let picked = apply(
            &pixels,
            &DitherSettings {
                colors: DitherColors::TwoColors,
                dark: [0.2, 0.0, 0.0],
                light: [0.0, 0.4, 1.0],
                levels: 8.0,
                ..quiet()
            },
        );
        let pixel = picked.get_pixel(6, 0).0;
        assert!(
            pixel[2] > pixel[1] && pixel[1] > pixel[0],
            "the light colour is the blue one, mixed into the dark red: {pixel:?}"
        );
        let original = apply(
            &RgbaImage::from_pixel(8, 4, Rgba([200, 40, 90, 255])),
            &DitherSettings {
                colors: DitherColors::Original,
                levels: 8.0,
                ..quiet()
            },
        );
        let kept = original.get_pixel(0, 0).0;
        assert!(
            kept[0] > kept[2] && kept[2] >= kept[1] && kept[0] > 100,
            "the image's own colour survives, as a tone of it: {kept:?}"
        );
    }

    #[test]
    fn chunky_pixels_average_down_and_come_back_whole() {
        let pixels = RgbaImage::from_fn(8, 8, |x, y| {
            let on = (x < 4) == (y < 4);
            Rgba([if on { 255 } else { 0 }; 4])
        });
        let chunky = apply(
            &pixels,
            &DitherSettings {
                pixel_size: 4.0,
                ..Default::default()
            },
        );
        assert_eq!(chunky.dimensions(), pixels.dimensions());
        // Four by four blocks, each one colour: nearest, not smoothed.
        let top_left = chunky.get_pixel(0, 0).0;
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(chunky.get_pixel(x, y).0, top_left, "block {x},{y}");
            }
        }
        assert_ne!(
            chunky.get_pixel(0, 0).0[0],
            chunky.get_pixel(6, 0).0[0],
            "and the blocks differ, as the picture does"
        );
        // A size of one leaves the picture alone in that respect.
        let single = apply(
            &pixels,
            &DitherSettings {
                pixel_size: 1.0,
                ..Default::default()
            },
        );
        assert!(
            single
                .pixels()
                .filter(|p| p.0[0] != chunky.get_pixel(0, 0).0[0])
                .count()
                > 0,
            "no blocking, so the edges survive"
        );
    }

    #[test]
    fn the_dot_shape_erodes_each_chunky_pixel_to_a_circle() {
        let pixels = RgbaImage::from_fn(8, 8, |_, _| Rgba([255, 255, 255, 255]));
        let square = apply(
            &pixels,
            &DitherSettings {
                pixel_size: 4.0,
                levels: 8.0,
                ..Default::default()
            },
        );
        let dots = apply(
            &pixels,
            &DitherSettings {
                pixel_size: 4.0,
                pixel_shape: PixelShape::Dot,
                levels: 8.0,
                ..Default::default()
            },
        );
        // The centre of a four-pixel block survives; its corner is gap.
        assert!(
            dots.get_pixel(2, 2).0[0] > 200,
            "the middle of the dot is lit: {:?}",
            dots.get_pixel(2, 2).0
        );
        assert!(
            dots.get_pixel(0, 0).0[0] < 60,
            "and the corner shows the dark colour: {:?}",
            dots.get_pixel(0, 0).0
        );
        assert_eq!(
            square.get_pixel(0, 0).0[0],
            255,
            "a square keeps its corner"
        );
        // With no chunky pixels there is nothing to erode, and nothing happens.
        let untouched = apply(
            &pixels,
            &DitherSettings {
                pixel_size: 1.0,
                pixel_shape: PixelShape::Dot,
                ..Default::default()
            },
        );
        let squared = apply(
            &pixels,
            &DitherSettings {
                pixel_size: 1.0,
                ..Default::default()
            },
        );
        assert_eq!(untouched, squared);
    }

    #[test]
    fn ascii_draws_characters_and_ignores_the_pixel_size() {
        let pixels = RgbaImage::from_fn(64, 32, |x, _| {
            let level = (x * 4).min(255) as u8;
            Rgba([level, level, level, 255])
        });
        let text = apply(
            &pixels,
            &DitherSettings {
                style: DitherStyle::Ascii,
                pixel_size: 8.0,
                text_size: 14.0,
                ..quiet()
            },
        );
        // Characters are drawn at full resolution, so the pixel size cannot have
        // blocked them.
        let distinct: std::collections::HashSet<u8> =
            text.pixels().map(|pixel| pixel.0[0]).collect();
        assert!(
            distinct.len() > 2,
            "more than black and white: {} shades",
            distinct.len()
        );
        // A tone ramp left to right picks a denser character, so the ink grows
        // with the tone.
        let ink = |image: &RgbaImage, at: (u32, u32)| -> u32 {
            (0..image.height())
                .map(|y| u32::from(image.get_pixel(at.0, y).0[0] > 127))
                .sum()
        };
        let left = ink(&text, (2, 0));
        let right = ink(&text, (60, 0));
        assert!(
            right > left,
            "the right of the ramp is inkier: {left} against {right}"
        );
    }

    #[test]
    fn the_style_groups_list_every_style_once() {
        let listed: Vec<DitherStyle> = DitherStyle::GROUPS
            .iter()
            .flat_map(|(_, styles)| styles.iter().copied())
            .collect();
        assert_eq!(
            listed,
            DitherStyle::ALL.to_vec(),
            "the panel's four groups are the ten styles in order, each once"
        );
        // The first three groups are headed, as upstream's are; the last is not.
        assert_eq!(
            DitherStyle::GROUPS
                .iter()
                .map(|(heading, _)| *heading)
                .collect::<Vec<_>>(),
            vec![
                Some("Error diffusion"),
                Some("Ordered"),
                Some("Halftone"),
                None
            ]
        );
    }

    #[test]
    fn settings_survive_a_save_and_take_defaults_for_what_is_missing() {
        let settings = DitherSettings {
            style: DitherStyle::HalftoneDiamonds,
            pixel_size: 3.0,
            cell_size: 12.0,
            angle: 30.0,
            levels: 6.0,
            colors: DitherColors::Original,
            characters: "#+.".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(
            serde_json::from_str::<DitherSettings>(&json).unwrap(),
            settings
        );
        // A settings blob written before a control existed opens with that
        // control at Compositor's own default rather than at zero.
        let older: DitherSettings = serde_json::from_str(r#"{"style":"Ascii"}"#).unwrap();
        assert_eq!(older.style, DitherStyle::Ascii);
        let defaults = DitherSettings::default();
        assert_eq!(older.pixel_size, defaults.pixel_size);
        assert_eq!(older.levels, defaults.levels);
        assert_eq!(older.colors, defaults.colors);
        assert!(older.light_on_dark);
    }

    #[test]
    fn settings_are_clamped_the_way_the_panel_clamps_them() {
        let settings = DitherSettings {
            pixel_size: 100.0,
            cell_size: 1.0,
            text_size: 0.0,
            angle: 400.0,
            levels: 40.0,
            diffusion: -20.0,
            density: f32::NAN,
            contrast: 1000.0,
            dark: [-1.0, 0.5, 2.0],
            light: [f32::INFINITY, 0.25, -0.5],
            characters: "ab\ncd\u{1}ef".into(),
            ..Default::default()
        }
        .normalized();
        assert_eq!(settings.pixel_size, 32.0);
        assert_eq!(settings.cell_size, 4.0);
        assert_eq!(settings.text_size, 6.0);
        assert_eq!(settings.angle, 90.0);
        assert_eq!(settings.levels, 8.0);
        assert_eq!(settings.diffusion, 0.0);
        assert_eq!(
            settings.density, 0.0,
            "a number that is not a number falls back"
        );
        assert_eq!(settings.contrast, 100.0);
        assert_eq!(settings.dark, [0.0, 0.5, 1.0]);
        assert_eq!(settings.light, [1.0, 0.25, 0.0]);
        assert_eq!(
            settings.characters, "abcdef",
            "newlines and control codes are dropped"
        );
        // A fractional pixel size is a whole number of pixels.
        assert_eq!(
            DitherSettings {
                pixel_size: 3.6,
                ..Default::default()
            }
            .normalized()
            .pixel_size,
            4.0
        );
    }

    /// A preview hands the kernel a smaller copy of the layer, and the sizes the
    /// user picks are distances in the real layer, so they have to be divided by
    /// how far the copy was scaled down. Left undivided, a preview would come out
    /// blockier than what Apply writes.
    #[test]
    fn a_preview_asks_for_the_sizes_of_the_copy_it_is_given() {
        let settings = DitherSettings {
            pixel_size: 8.0,
            cell_size: 12.0,
            text_size: 20.0,
            ..Default::default()
        }
        .normalized();
        // At full resolution the sizes are the user's own.
        let full = settings.scaled_for(1.0);
        assert_eq!(
            (full.pixel_size, full.cell_size, full.text_size),
            (8.0, 12.0, 20.0)
        );
        // A quarter-size copy asks for a quarter of each.
        let quarter = settings.scaled_for(4.0);
        assert_eq!(
            (quarter.pixel_size, quarter.cell_size, quarter.text_size),
            (2.0, 3.0, 5.0)
        );
        // Below the copy's own pixel there is nothing smaller to ask for, and a
        // halftone cell never falls under the two the kernel works in.
        let tiny = DitherSettings {
            pixel_size: 1.0,
            cell_size: 4.0,
            text_size: 6.0,
            ..Default::default()
        }
        .normalized()
        .scaled_for(8.0);
        assert_eq!(
            (tiny.pixel_size, tiny.cell_size, tiny.text_size),
            (1.0, 2.0, 1.0)
        );
        // A scale that is not a positive number is no scale at all, rather than
        // a way to turn every size into an infinity or a nought.
        for bad in [0.0, -2.0, f32::NAN, f32::INFINITY] {
            let kept = settings.scaled_for(bad);
            assert_eq!(
                (kept.pixel_size, kept.cell_size, kept.text_size),
                (8.0, 12.0, 20.0),
                "{bad} leaves the sizes alone"
            );
        }
    }

    /// And on the picture: a chunky pixel covers the same part of the layer
    /// whichever copy the kernel was handed, so a preview is blocked where Apply
    /// will block it. The layer here is eight-pixel bands of black and white, so
    /// a block that spans two bands shows up as a grey that a correct pass would
    /// never make.
    #[test]
    fn the_same_chunky_pixels_come_out_of_a_scaled_copy() {
        let bands = RgbaImage::from_fn(64, 16, |x, _| {
            let level = if (x / 8) % 2 == 0 { 0 } else { 255 };
            Rgba([level, level, level, 255])
        });
        // Every aligned block of `block` pixels is one colour.
        let tiled = |image: &RgbaImage, block: u32| -> bool {
            image.pixels().enumerate().all(|(index, pixel)| {
                let (x, y) = (index as u32 % image.width(), index as u32 / image.width());
                *pixel == *image.get_pixel(x / block * block, y / block * block)
            })
        };
        let settings = DitherSettings {
            pixel_size: 8.0,
            levels: 8.0,
            ..Default::default()
        };
        let full = apply(&bands, &settings);
        assert!(tiled(&full, 8), "eight-pixel blocks at full resolution");
        assert!(
            !tiled(&full, 16),
            "and they stop there, so the bands survive"
        );

        // Half the layer: the eight-pixel block is four of the copy's.
        let half = RgbaImage::from_fn(32, 8, |x, y| *bands.get_pixel(x * 2, y * 2));
        assert_eq!(half.dimensions(), (32, 8));
        let preview = apply_at_scale(&half, &settings, 2.0, &AtomicBool::new(false));
        assert_eq!(preview.dimensions(), half.dimensions());
        assert!(tiled(&preview, 4), "four-pixel blocks at half resolution");
        assert!(
            !tiled(&preview, 8),
            "and the pass did not keep the panel's eight, which would \
             have averaged a black band into a white one"
        );
    }

    /// A pass the user has moved past stops at the next scanline and hands the
    /// layer back, so a cancelled preview can never reach the document.
    #[test]
    fn a_called_off_pass_hands_the_layer_back_as_it_was() {
        let pixels = ramp();
        for (style, pixel_size, pixel_shape) in [
            (DitherStyle::Atkinson, 1.0, PixelShape::Square),
            (DitherStyle::FloydSteinberg, 1.0, PixelShape::Square),
            (DitherStyle::Bayer4, 1.0, PixelShape::Square),
            (DitherStyle::HalftoneDots, 1.0, PixelShape::Square),
            (DitherStyle::MacPatterns, 4.0, PixelShape::Square),
            (DitherStyle::Ascii, 1.0, PixelShape::Square),
            (DitherStyle::Atkinson, 4.0, PixelShape::Dot),
        ] {
            let settings = DitherSettings {
                style,
                pixel_size,
                pixel_shape,
                ..Default::default()
            };
            let called_off = AtomicBool::new(true);
            assert_eq!(
                apply_at_scale(&pixels, &settings, 1.0, &called_off),
                pixels,
                "{style:?} hands the layer back when it is called off"
            );
            // The same settings left alone still dither, so the check above is
            // not simply an early return for every pass.
            let live = AtomicBool::new(false);
            assert_ne!(
                apply_at_scale(&pixels, &settings, 1.0, &live),
                pixels,
                "{style:?} dithers when nothing calls it off"
            );
        }
    }

    /// The planes the kernel keeps, which is what a caller measures a layer
    /// against the machine's memory with.
    #[test]
    fn the_working_set_covers_every_plane_the_kernel_keeps() {
        // The two-colour modes keep one tone plane beside the alpha and the
        // result; Original keeps one per channel, the image's own colours, and
        // the marks beside them.
        assert_eq!(bytes_per_pixel(DitherColors::BlackWhite), 1 + 4 + 4);
        assert_eq!(bytes_per_pixel(DitherColors::TwoColors), 1 + 4 + 4);
        assert_eq!(
            bytes_per_pixel(DitherColors::Original),
            12 + 4 * 3 + 1 + 4 + 4
        );
        assert_eq!(working_set(1_000_000, DitherColors::TwoColors), 9_000_000);
        assert_eq!(working_set(1_000_000, DitherColors::Original), 33_000_000);
        // A layer large enough to overflow the multiplication saturates rather
        // than wrapping into a small number the guard would wave through.
        assert_eq!(
            working_set(u64::MAX, DitherColors::Original),
            u64::MAX,
            "an absurd layer is refused, not measured as a small one"
        );
    }
}
