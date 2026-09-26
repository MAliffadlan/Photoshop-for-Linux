use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use cosmic_text::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight, Wrap,
};
use image::{Pixel, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::document::{Layer, MAX_SIDE, Point, validate_size};

pub const MAX_TEXT_BYTES: usize = 16_384;
/// How many separately coloured ranges one text layer may hold.
pub const MAX_COLOR_RUNS: usize = 1_024;
const FALLBACK_FAMILY: &str = "Inter Variable";
/// The leading a plain text layer uses, matching the multiplier a paragraph box defaults to.
const DEFAULT_LINE_SPACING: f32 = 1.3;
const DEFAULT_BOX_WIDTH: f32 = 512.0;
const MAX_LINE_SPACING: f32 = 4.0;
const MIN_LINE_SPACING: f32 = 0.5;
const MAX_PARAGRAPH_SPACING: f32 = 2_000.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl TextAlign {
    /// How much of the leftover line width moves each line, as a fraction.
    fn factor(self) -> f32 {
        match self {
            Self::Left => 0.0,
            Self::Center => 0.5,
            Self::Right => 1.0,
        }
    }
}

/// A paragraph box: text wraps to `width`, `min_height` reserves space the text may grow past.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextBox {
    #[serde(default = "default_box_width")]
    pub width: f32,
    #[serde(default)]
    pub min_height: f32,
    #[serde(default)]
    pub align: TextAlign,
    #[serde(default = "default_line_spacing")]
    pub line_spacing: f32,
    #[serde(default)]
    pub paragraph_spacing: f32,
}

impl Default for TextBox {
    fn default() -> Self {
        Self {
            width: DEFAULT_BOX_WIDTH,
            min_height: 0.0,
            align: TextAlign::default(),
            line_spacing: DEFAULT_LINE_SPACING,
            paragraph_spacing: 0.0,
        }
    }
}

impl TextBox {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.width.is_finite() && (1.0..=MAX_SIDE as f32).contains(&self.width),
            "Text box width must be between 1 and {MAX_SIDE} pixels"
        );
        ensure!(
            self.min_height.is_finite() && (0.0..=MAX_SIDE as f32).contains(&self.min_height),
            "Text box height must be between 0 and {MAX_SIDE} pixels"
        );
        ensure!(
            self.line_spacing.is_finite()
                && (MIN_LINE_SPACING..=MAX_LINE_SPACING).contains(&self.line_spacing),
            "Line spacing must be between {MIN_LINE_SPACING} and {MAX_LINE_SPACING}"
        );
        ensure!(
            self.paragraph_spacing.is_finite()
                && (0.0..=MAX_PARAGRAPH_SPACING).contains(&self.paragraph_spacing),
            "Paragraph spacing must be between 0 and {MAX_PARAGRAPH_SPACING} pixels"
        );
        Ok(())
    }
}

fn default_box_width() -> f32 {
    DEFAULT_BOX_WIDTH
}

fn default_line_spacing() -> f32 {
    DEFAULT_LINE_SPACING
}

/// Letters painted in their own colour, as Compositor 1.3.2 stores them: offsets
/// in UTF-16 code units, because that is what a text editor counts, so a range
/// keeps its meaning for text outside the basic multilingual plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextColorRun {
    /// The first UTF-16 code unit of the run within the content.
    pub start: u32,
    /// How many UTF-16 code units the run covers.
    pub length: u32,
    /// The colour those letters take. The layer's own alpha still applies, as
    /// it does to every other pixel of the text.
    pub color: [u8; 4],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub content: String,
    pub family: String,
    pub size: f32,
    pub color: [u8; 4],
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    /// Ranges of letters that take a colour other than the layer's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub color_runs: Vec<TextColorRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#box: Option<TextBox>,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            content: "Text".into(),
            family: FALLBACK_FAMILY.into(),
            size: 48.0,
            color: [0, 0, 0, 255],
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            color_runs: Vec::new(),
            r#box: None,
        }
    }
}

impl TextStyle {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.content.len() <= MAX_TEXT_BYTES,
            "Text is limited to 16 KiB"
        );
        ensure!(
            !self.family.trim().is_empty() && self.family.len() <= 1024,
            "Invalid font family"
        );
        ensure!(
            self.size.is_finite() && (1.0..=1024.0).contains(&self.size),
            "Font size must be between 1 and 1024 pixels"
        );
        ensure!(
            self.color_runs.len() <= MAX_COLOR_RUNS,
            "A text layer is limited to {MAX_COLOR_RUNS} coloured ranges"
        );
        let units = self.content.encode_utf16().count();
        let mut previous_end = 0;
        for run in &self.color_runs {
            ensure!(run.length > 0, "A coloured text range is empty");
            let end = run
                .start
                .checked_add(run.length)
                .context("Coloured text range overflow")?;
            ensure!(
                run.start >= previous_end,
                "Coloured text ranges overlap or are out of order"
            );
            ensure!(
                end as usize <= units,
                "A coloured text range is past the end of the text"
            );
            previous_end = end;
        }
        if let Some(text_box) = &self.r#box {
            text_box.validate()?;
        }
        Ok(())
    }

    /// The colour of the letter at UTF-16 code unit `index`, which is the
    /// layer's own colour unless a run covers it.
    pub fn color_at(&self, index: u32) -> [u8; 4] {
        self.color_runs
            .iter()
            .find(|run| run.start <= index && index < run.start + run.length)
            .map_or(self.color, |run| run.color)
    }

    /// Give every letter in `start..start + length` the colour, replacing any
    /// run that overlaps it, and leaving the rest of the text as it was.
    pub fn set_color_run(&mut self, start: u32, length: u32, color: [u8; 4]) {
        let end = start.saturating_add(length);
        self.color_runs
            .retain(|run| run.start + run.length <= start || run.start >= end);
        self.color_runs.push(TextColorRun {
            start,
            length,
            color,
        });
        self.color_runs.sort_by_key(|run| run.start);
    }

    /// Drop the letter colours, so the whole text is the layer's own colour.
    pub fn clear_color_runs(&mut self) {
        self.color_runs.clear();
    }

    pub fn line_spacing(&self) -> f32 {
        self.r#box
            .map_or(DEFAULT_LINE_SPACING, |text_box| text_box.line_spacing)
    }

    pub fn layer_name(&self) -> String {
        let name: String = self
            .content
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(60)
            .collect();
        if name.is_empty() { "Text".into() } else { name }
    }
}

/// The UTF-16 offsets of a character range in `text`, which is how a colour run
/// counts. Both ends are clamped to the text and an end before the start is
/// swapped, so a selection made by a text editor can be handed over as it is.
pub fn utf16_range(text: &str, start: usize, end: usize) -> (u32, u32) {
    let mut utf16 = 0_u32;
    let mut begin = None;
    let mut finish = 0;
    for (index, ch) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if index >= start {
            begin.get_or_insert(utf16);
        }
        if index >= end {
            finish = utf16;
            break;
        }
        utf16 += ch.len_utf16() as u32;
    }
    (begin.unwrap_or(utf16), finish)
}

/// The byte offset of every UTF-16 code unit in `text`, plus one past the end,
/// so that a colour run's offsets can be turned into the byte range the layout
/// reports. A character outside the basic multilingual plane is one code unit
/// for each of its two halves, and both name the same byte.
fn utf16_offsets(text: &str) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(text.len() / 2 + 2);
    for (byte, ch) in text.char_indices() {
        offsets.push(byte);
        if ch.len_utf16() == 2 {
            offsets.push(byte);
        }
    }
    offsets.push(text.len());
    offsets
}

/// The byte range of the content a colour run covers, or `None` when the run
/// points past the text.
fn color_run_bytes(offsets: &[usize], run: &TextColorRun) -> Option<(usize, usize)> {
    let start = run.start as usize;
    let end = start.checked_add(run.length as usize)?;
    Some((*offsets.get(start)?, *offsets.get(end)?))
}

/// One font database per editor, loaded lazily when the text tool is first used.
pub struct TextRenderer {
    fonts: FontSystem,
    families: Vec<String>,
}

impl Default for TextRenderer {
    fn default() -> Self {
        let fonts = FontSystem::new_with_fonts([cosmic_text::fontdb::Source::Binary(Arc::new(
            include_bytes!("../assets/fonts/InterVariable.ttf").to_vec(),
        ))]);
        Self::with_fonts(fonts)
    }
}

impl TextRenderer {
    fn with_fonts(fonts: FontSystem) -> Self {
        let mut families: Vec<_> = fonts
            .db()
            .faces()
            .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
            .collect();
        families.sort_by_key(|name| name.to_lowercase());
        families.dedup();
        Self { fonts, families }
    }

    pub fn families(&self) -> &[String] {
        &self.families
    }

    pub fn has_family(&self, family: &str) -> bool {
        self.families
            .iter()
            .any(|name| name.eq_ignore_ascii_case(family))
    }

    pub fn render(&mut self, style: &TextStyle) -> Result<RgbaImage> {
        style.validate()?;
        let line_height = style.size * style.line_spacing();
        ensure!(
            (style.content.lines().count().max(1) as f32 * line_height) <= 30_000.0,
            "Text is too tall"
        );
        let family = if self.has_family(&style.family) {
            &style.family
        } else {
            FALLBACK_FAMILY
        };
        // Resolve the closest available face first. Requesting a missing weight or
        // style directly can substitute an unrelated family during shaping.
        let face = self
            .fonts
            .db()
            .query(&cosmic_text::fontdb::Query {
                families: &[Family::Name(family)],
                weight: if style.bold {
                    Weight::BOLD
                } else {
                    Weight::NORMAL
                },
                style: if style.italic {
                    Style::Italic
                } else {
                    Style::Normal
                },
                ..Default::default()
            })
            .and_then(|id| self.fonts.db().face(id))
            .ok_or_else(|| anyhow::anyhow!("The font could not be loaded"))?;
        let attrs = Attrs::new()
            .family(Family::Name(family))
            .weight(face.weight)
            .style(face.style)
            .stretch(face.stretch);
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(style.size, line_height));
        // A paragraph box wraps at its width; plain text only breaks where the author did.
        match style.r#box {
            Some(text_box) => {
                buffer.set_wrap(&mut self.fonts, Wrap::Word);
                buffer.set_size(&mut self.fonts, Some(text_box.width), None);
            }
            None => {
                buffer.set_wrap(&mut self.fonts, Wrap::None);
                buffer.set_size(&mut self.fonts, None, None);
            }
        }
        buffer.set_text(&mut self.fonts, &style.content, &attrs, Shaping::Advanced);
        // Boxes centre or right-align each wrapped line and open a gap between paragraphs.
        let shift = |run: &cosmic_text::LayoutRun| -> (f32, f32) {
            style.r#box.map_or((0.0, 0.0), |text_box| {
                (
                    text_box.align.factor() * (text_box.width - run.line_w),
                    text_box.paragraph_spacing * run.line_i as f32,
                )
            })
        };

        let mut right = 1.0_f32;
        let mut bottom = line_height;
        for run in buffer.layout_runs() {
            let (x, y) = shift(&run);
            right = right.max(run.line_w + x);
            bottom = bottom.max(run.line_top + run.line_height + y);
        }
        validate_size(right.ceil() as u32, bottom.ceil() as u32)?;

        // Measure actual ink as well as advances so italic overhangs and combining
        // marks are preserved. Keep the glyph cache local to bound retained memory.
        let mut cache = SwashCache::new();
        let (mut left, mut top) = (0, 0);
        let (mut right, mut bottom) = (right.ceil() as i32, bottom.ceil() as i32);
        // Colour runs are UTF-16 offsets while glyphs report byte offsets into
        // their own line, so the runs are resolved to byte ranges once here.
        let mut color_runs = Vec::new();
        if !style.color_runs.is_empty() {
            let offsets = utf16_offsets(&style.content);
            // Validation refuses a run that points past the text, so one that
            // cannot be resolved here has nowhere to paint and is dropped.
            color_runs = style
                .color_runs
                .iter()
                .filter_map(|run| {
                    let (start, end) = color_run_bytes(&offsets, run)?;
                    Some((start, end, run.color))
                })
                .collect();
        }
        // Glyphs report a byte offset into the line they belong to, and a
        // wrapped line is reported once per row, so the line's place in the
        // content is found once per line and reused by its rows.
        let mut cursor = 0;
        let mut line = None;
        let mut glyphs = Vec::new();
        let mut rules = Vec::new();
        for run in buffer.layout_runs() {
            if line.map(|(index, _): (usize, usize)| index) != Some(run.line_i) {
                let at = style.content[cursor..]
                    .find(run.text)
                    .map_or(cursor, |at| cursor + at);
                cursor = at + run.text.len();
                line = Some((run.line_i, at));
            }
            let found = line.map_or(0, |(_, at)| at);
            let (offset_x, offset_y) = shift(&run);
            let (offset_x, offset_y) = (offset_x.round() as i32, offset_y.round() as i32);
            for glyph in run.glyphs {
                let mut physical = glyph.physical((0.0, 0.0), 1.0);
                if style.italic
                    && self
                        .fonts
                        .db()
                        .face(glyph.font_id)
                        .is_some_and(|face| face.style == Style::Normal)
                {
                    physical.cache_key.flags |= cosmic_text::CacheKeyFlags::FAKE_ITALIC;
                }
                let y = run.line_y as i32 + physical.y;
                // Families without a bold face still have a visible bold style.
                let embolden = if style.bold
                    && self
                        .fonts
                        .db()
                        .face(glyph.font_id)
                        .is_some_and(|face| face.weight < Weight::SEMIBOLD)
                {
                    (style.size * 0.025).ceil() as i32
                } else {
                    0
                };
                if let Some(image) = cache.get_image(&mut self.fonts, physical.cache_key) {
                    let placement = image.placement;
                    let x = physical.x + placement.left + offset_x;
                    let y = y - placement.top + offset_y;
                    left = left.min(x);
                    top = top.min(y);
                    right = right.max(x + placement.width as i32 + embolden);
                    bottom = bottom.max(y + placement.height as i32);
                }
                let index = found + glyph.start;
                let color = color_runs
                    .iter()
                    .find(|(start, end, _)| index >= *start && index < *end)
                    .map_or(style.color, |(_, _, color)| *color);
                glyphs.push((physical, y, embolden, offset_x, offset_y, color));
            }
            let thickness = (style.size / 16.0).max(1.0);
            for (enabled, y) in [
                (style.underline, run.line_y + style.size * 0.1),
                (style.strikethrough, run.line_y - style.size * 0.3),
            ] {
                if enabled && run.line_w > 0.0 {
                    let y = y + offset_y as f32;
                    top = top.min(y.floor() as i32);
                    bottom = bottom.max((y + thickness).ceil() as i32);
                    rules.push((run.line_w + offset_x as f32, y, thickness));
                }
            }
        }
        // A box keeps its own area, so the raster is the box rather than the ink it holds.
        let (width, height, left, top): (u32, u32, i32, i32) = match style.r#box {
            Some(text_box) => {
                let width = text_box.width.round().max(1.0) as u32;
                let height = bottom.max(text_box.min_height.ceil() as i32).max(1) as u32;
                validate_size(width, height)?;
                (width, height, 0, 0)
            }
            None => {
                let (width, height) = ((right - left) as u32, (bottom - top) as u32);
                validate_size(width, height)?;
                (width, height, left, top)
            }
        };
        let mut pixels = RgbaImage::new(width, height);
        let color = Color::rgb(style.color[0], style.color[1], style.color[2]);
        for (glyph, baseline, embolden, offset_x, offset_y, glyph_color) in glyphs {
            let color = Color::rgb(glyph_color[0], glyph_color[1], glyph_color[2]);
            cache.with_pixels(&mut self.fonts, glyph.cache_key, color, |x, y, color| {
                for offset in 0..=embolden {
                    if let Some(pixel) = pixels.get_pixel_mut_checked(
                        (glyph.x + x + offset + offset_x - left) as u32,
                        (baseline + offset_y + y - top) as u32,
                    ) {
                        pixel.blend(&Rgba(color.as_rgba()));
                    }
                }
            });
        }
        for (width, y, thickness) in rules {
            for py in y.floor() as i32..(y + thickness).ceil() as i32 {
                for px in 0..width.ceil() as i32 {
                    let coverage = (width - px as f32).min(1.0)
                        * ((y + thickness).min(py as f32 + 1.0) - y.max(py as f32));
                    let mut rgba = color.as_rgba();
                    rgba[3] = (coverage * 255.0).round() as u8;
                    pixels
                        .get_pixel_mut((px - left) as u32, (py - top) as u32)
                        .blend(&Rgba(rgba));
                }
            }
        }
        for pixel in pixels.pixels_mut() {
            pixel[3] = ((u16::from(pixel[3]) * u16::from(style.color[3]) + 127) / 255) as u8;
        }
        Ok(pixels)
    }
}

/// Replace the text while retaining the layer's scale, rotation, and top-left anchor.
pub fn update_layer(layer: &mut Layer, style: TextStyle, pixels: RgbaImage) -> Result<()> {
    style.validate()?;
    ensure!(
        !layer.locked && layer.text.is_some(),
        "Select an unlocked text layer"
    );
    let old = layer
        .pixels
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Text layer has no pixels"))?;
    let anchor = layer.transform.point(Point::new(0.0, 0.0));
    let mut transform = layer.transform;
    if style.r#box.is_some() {
        // A paragraph box owns its area, so the layer is exactly the box.
        transform.width = pixels.width() as f32;
        transform.height = pixels.height() as f32;
    } else {
        transform.width *= pixels.width() as f32 / old.width() as f32;
        transform.height *= pixels.height() as f32 / old.height() as f32;
    }
    let moved = transform.point(Point::new(0.0, 0.0));
    transform.x += anchor.x - moved.x;
    transform.y += anchor.y - moved.y;
    ensure!(transform.valid(), "Text transform is too large");
    if layer
        .text
        .as_ref()
        .is_some_and(|old| old.layer_name() == layer.name)
    {
        layer.name = style.layer_name();
    }
    layer.set_transform(transform);
    layer.pixels = Some(Arc::new(pixels));
    layer.text = Some(style);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::{Document, Mask},
        io, paint, render,
    };

    fn renderer() -> TextRenderer {
        let mut db = cosmic_text::fontdb::Database::new();
        db.load_font_data(include_bytes!("../assets/fonts/InterVariable.ttf").to_vec());
        TextRenderer::with_fonts(FontSystem::new_with_locale_and_db("en-US".into(), db))
    }

    fn layer(renderer: &mut TextRenderer, style: TextStyle) -> Layer {
        let mut layer = Layer::image(style.layer_name(), renderer.render(&style).unwrap());
        layer.text = Some(style);
        layer
    }

    #[test]
    fn font_discovery_includes_installed_families_and_bundled_fallback() {
        let renderer = TextRenderer::default();
        assert!(renderer.has_family(FALLBACK_FAMILY));
        for face in renderer.fonts.db().faces() {
            for (family, _) in &face.families {
                assert!(renderer.has_family(family));
            }
        }
        let mut fallback = self::renderer();
        let missing = TextStyle {
            family: "Definitely unavailable mectov test font".into(),
            ..Default::default()
        };
        assert_eq!(
            fallback.render(&missing).unwrap(),
            fallback.render(&TextStyle::default()).unwrap()
        );
    }

    #[test]
    fn styles_change_ink_and_preserve_color_alpha_and_multiline_layout() {
        let mut renderer = renderer();
        let style = TextStyle {
            content: "Office fj Å\nSecond line".into(),
            color: [30, 110, 190, 128],
            ..Default::default()
        };
        let plain = renderer.render(&style).unwrap();
        assert!(plain.height() >= (style.size * 2.6) as u32);
        assert!(plain.pixels().any(|pixel| pixel[3] == 128));
        assert!(plain.pixels().any(|pixel| (1..128).contains(&pixel[3])));
        for index in 0..4 {
            let mut decorated = style.clone();
            match index {
                0 => decorated.bold = true,
                1 => decorated.italic = true,
                2 => decorated.underline = true,
                _ => decorated.strikethrough = true,
            }
            let pixels = renderer.render(&decorated).unwrap();
            assert_ne!(
                pixels, plain,
                "Decoration {index} must affect rendered text"
            );
            for pixel in pixels.pixels().filter(|pixel| pixel[3] > 0) {
                assert!(pixel[3] <= 128);
                for channel in 0..3 {
                    assert!(
                        (i16::from(pixel[channel]) - i16::from(style.color[channel])).abs() <= 1
                    );
                }
            }
        }
    }

    #[test]
    fn colour_runs_paint_only_the_letters_they_cover() {
        let mut renderer = renderer();
        let style = TextStyle {
            content: "Office fj Å\nSecond".into(),
            color: [30, 110, 190, 255],
            ..Default::default()
        };
        // "fj Å" sits at UTF-16 units 7..12 of the first line, and the emoji
        // outside the basic multilingual plane counts as two units.
        let red = TextColorRun {
            start: 7,
            length: 5,
            color: [220, 20, 20, 255],
        };
        style.validate().unwrap();
        let mut coloured = style.clone();
        coloured.color_runs = vec![red];
        coloured.validate().unwrap();
        assert_eq!(
            style.color_at(7),
            style.color,
            "the plain style has no runs"
        );
        assert_eq!(coloured.color_at(0), style.color);
        assert_eq!(coloured.color_at(7), [220, 20, 20, 255]);
        assert_eq!(coloured.color_at(11), [220, 20, 20, 255]);
        assert_eq!(coloured.color_at(12), style.color);

        let plain = renderer.render(&style).unwrap();
        let painted = renderer.render(&coloured).unwrap();
        assert_ne!(painted, plain, "a colour run must change the ink");
        assert_eq!(painted.dimensions(), plain.dimensions());
        // The same letters are drawn, in the same places: only the colour of the
        // ones the run covers changes, and they leave the layer's colour behind.
        let ink = |image: &RgbaImage| image.pixels().filter(|pixel| pixel[3] > 0).count();
        let red = |image: &RgbaImage| {
            image
                .pixels()
                .filter(|pixel| pixel[0] > 150 && pixel[1] < 100 && pixel[2] < 100)
                .count()
        };
        let blue = |image: &RgbaImage| {
            image
                .pixels()
                .filter(|pixel| pixel[2] > 150 && pixel[0] < 100 && pixel[3] > 0)
                .count()
        };
        assert_eq!(ink(&painted), ink(&plain), "the same letters are drawn");
        assert!(red(&painted) > 20, "the run is painted red");
        assert_eq!(red(&plain), 0, "the plain text has no red in it");
        assert!(blue(&painted) < blue(&plain), "those letters left the blue");
    }

    #[test]
    fn colour_runs_are_checked_before_they_are_trusted() {
        let style = TextStyle {
            content: "Twelve chars".into(),
            ..Default::default()
        };
        let run = |start, length| TextColorRun {
            start,
            length,
            color: [1, 2, 3, 255],
        };
        let mut checked = style.clone();
        checked.color_runs = vec![run(0, 0)];
        assert!(checked.validate().is_err(), "an empty run paints nothing");
        checked.color_runs = vec![run(0, 14)];
        assert!(checked.validate().is_err(), "a run past the end");
        checked.color_runs = vec![run(4, 4), run(2, 3)];
        assert!(checked.validate().is_err(), "runs out of order");
        checked.color_runs = vec![run(0, 6), run(4, 4)];
        assert!(checked.validate().is_err(), "runs overlap");
        checked.color_runs = vec![run(0, 6), run(6, 6)];
        checked.validate().unwrap();
        checked.color_runs = vec![run(u32::MAX, 2)];
        assert!(checked.validate().is_err(), "a run that overflows");
        checked.color_runs = vec![run(0, 1); MAX_COLOR_RUNS + 1];
        assert!(checked.validate().is_err(), "too many runs");
        // A file that leaves the field out is the layer's own colour throughout.
        let plain = TextStyle {
            content: "Twelve chars".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&plain).unwrap();
        assert!(
            !json.contains("color_runs"),
            "the field is left out: {json}"
        );
        let parsed: TextStyle = serde_json::from_str(&json).unwrap();
        assert!(parsed.color_runs.is_empty());
        assert_eq!(parsed, plain);
    }

    #[test]
    fn setting_a_colour_run_replaces_what_it_covers() {
        let mut style = TextStyle {
            content: "0123456789".into(),
            ..Default::default()
        };
        style.set_color_run(2, 3, [10, 20, 30, 255]);
        assert_eq!(style.color_runs.len(), 1);
        // A run over the same letters replaces the earlier one instead of
        // stacking a second colour on them.
        style.set_color_run(3, 2, [40, 50, 60, 255]);
        assert_eq!(style.color_runs.len(), 1);
        assert_eq!(style.color_runs[0].start, 3);
        assert_eq!(style.color_at(2), style.color);
        assert_eq!(style.color_at(3), [40, 50, 60, 255]);
        // A run beside another one keeps both, in order.
        style.set_color_run(7, 2, [70, 80, 90, 255]);
        assert_eq!(style.color_runs.len(), 2);
        style.validate().unwrap();
        style.clear_color_runs();
        assert!(style.color_runs.is_empty());
    }

    #[test]
    fn a_wrapped_line_keeps_its_letters_coloured() {
        let mut renderer = renderer();
        let style = TextStyle {
            content: "First line that wraps\nsecond".into(),
            size: 24.0,
            color: [10, 10, 10, 255],
            r#box: Some(TextBox {
                width: 120.0,
                ..boxed(0.0, 0.0)
            }),
            ..Default::default()
        };
        let mut coloured = style.clone();
        // The second line starts after the first, which wraps into several rows
        // and is reported more than once, so this only passes if a line's place
        // in the content is found rather than assumed.
        let second = style.content.find("second").unwrap() as u32;
        coloured.color_runs = vec![TextColorRun {
            start: second,
            length: 6,
            color: [220, 20, 20, 255],
        }];
        coloured.validate().unwrap();
        let painted = renderer.render(&coloured).unwrap();
        let plain = renderer.render(&style).unwrap();
        assert_eq!(painted.dimensions(), plain.dimensions());
        assert_ne!(painted, plain);
        assert!(
            painted
                .pixels()
                .any(|pixel| pixel[0] > 150 && pixel[1] < 100 && pixel[2] < 100),
            "the second line is painted"
        );
    }

    #[test]
    fn empty_text_and_invalid_or_oversized_input_are_handled() {
        let mut renderer = renderer();
        let mut style = TextStyle {
            content: String::new(),
            ..Default::default()
        };
        assert!(
            renderer
                .render(&style)
                .unwrap()
                .pixels()
                .all(|pixel| pixel[3] == 0)
        );
        for size in [0.0, -1.0, f32::NAN, f32::INFINITY, 1025.0] {
            style.size = size;
            assert!(renderer.render(&style).is_err());
        }
        style.size = 1024.0;
        style.content = "W".repeat(100);
        assert!(renderer.render(&style).is_err());
        style.content = "line\n".repeat(100);
        assert!(renderer.render(&style).is_err());
        style.content = "a".repeat(MAX_TEXT_BYTES + 1);
        assert!(renderer.render(&style).is_err());
    }

    #[test]
    fn text_round_trips_with_pixels_and_remains_editable_after_transform() {
        let mut renderer = renderer();
        let mut layer = layer(&mut renderer, TextStyle::default());
        layer.transform.x = 20.0;
        layer.transform.y = 30.0;
        layer.transform.rotation = 25.0;
        layer.transform.width *= 1.5;
        layer.transform.height *= 0.75;
        layer.mask = Some(Mask::white());
        let anchor = layer.transform.point(Point::default());
        let mut style = layer.text.clone().unwrap();
        style.content = "Longer text\nwith decorations".into();
        style.underline = true;
        style.strikethrough = true;
        let pixels = renderer.render(&style).unwrap();
        let dimensions = pixels.dimensions();
        update_layer(&mut layer, style.clone(), pixels).unwrap();
        assert!(anchor.distance(layer.transform.point(Point::default())) < 0.001);
        assert!((layer.transform.width - dimensions.0 as f32 * 1.5).abs() < 0.001);
        assert!((layer.transform.height - dimensions.1 as f32 * 0.75).abs() < 0.001);
        let mut document = Document::new(600, 300).unwrap();
        document.insert(layer);
        let file = tempfile::NamedTempFile::new().unwrap();
        io::save(&document, file.path()).unwrap();
        let loaded = io::load(file.path()).unwrap();
        assert_eq!(loaded.active().unwrap().text, Some(style));
        assert_eq!(
            loaded.active().unwrap().pixels,
            document.active().unwrap().pixels
        );
        assert_eq!(render::render(&loaded), render::render(&document));

        let old = document.active_mut().unwrap();
        paint::ensure_pixels(old).unwrap();
        assert!(old.text.is_none());
        let mut json = serde_json::to_value(old).unwrap();
        json.as_object_mut().unwrap().remove("text");
        assert!(
            serde_json::from_value::<Layer>(json)
                .unwrap()
                .text
                .is_none()
        );
    }

    fn boxed(width: f32, min_height: f32) -> TextBox {
        TextBox {
            width,
            min_height,
            ..Default::default()
        }
    }

    fn ink_bounds(pixels: &RgbaImage) -> (u32, u32) {
        let (mut left, mut top) = (u32::MAX, u32::MAX);
        let (mut right, mut bottom) = (0, 0);
        for (x, y, pixel) in pixels.enumerate_pixels() {
            if pixel[3] > 0 {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
        if left > right {
            return (0, 0);
        }
        (right - left, bottom - top)
    }

    fn first_ink(pixels: &RgbaImage) -> Option<(u32, u32)> {
        pixels
            .enumerate_pixels()
            .find_map(|(x, y, pixel)| (pixel[3] > 0).then_some((x, y)))
    }

    #[test]
    fn a_paragraph_box_wraps_and_keeps_its_own_area() {
        let mut renderer = renderer();
        let content = "Wrapping needs enough words to need more than one line in the box";
        let plain = renderer
            .render(&TextStyle {
                content: content.into(),
                size: 24.0,
                ..Default::default()
            })
            .unwrap();
        let style = TextStyle {
            content: content.into(),
            size: 24.0,
            r#box: Some(boxed(200.0, 0.0)),
            ..Default::default()
        };
        let wrapped = renderer.render(&style).unwrap();
        assert_eq!(wrapped.width(), 200);
        assert!(
            wrapped.height() > plain.height(),
            "wrapped {} vs plain {}",
            wrapped.height(),
            plain.height()
        );
        // The ink never leaves the box, and the layer is the box rather than the ink.
        let (ink_width, _) = ink_bounds(&wrapped);
        assert!(ink_width <= 200);
        assert!(ink_width < 200, "wrapped lines should not fill the box");
        let layer = layer(&mut renderer, style.clone());
        assert_eq!(layer.transform.width, 200.0);
        assert_eq!(layer.transform.height, wrapped.height() as f32);

        // A box wide enough for the sentence needs only one line.
        let mut wide = style.clone();
        wide.r#box = Some(boxed(4_000.0, 0.0));
        let single = renderer.render(&wide).unwrap();
        assert_eq!(single.width(), 4_000);
        assert!(single.height() < wrapped.height());
    }

    #[test]
    fn box_height_reserves_space_and_grows_past_it() {
        let mut renderer = renderer();
        let base = TextStyle {
            content: "Short".into(),
            size: 20.0,
            r#box: Some(boxed(300.0, 0.0)),
            ..Default::default()
        };
        let fitted = renderer.render(&base).unwrap();
        assert_eq!(fitted.width(), 300);
        assert!(
            ink_bounds(&fitted).1 < 400,
            "the text is shorter than the box"
        );
        let mut roomy = base.clone();
        roomy.r#box = Some(boxed(300.0, 400.0));
        let reserved = renderer.render(&roomy).unwrap();
        assert_eq!(reserved.width(), 300);
        assert_eq!(reserved.height(), 400);
        assert!(ink_bounds(&reserved).1 < 400, "the extra room stays empty");

        // Content taller than the minimum grows the box instead of clipping.
        let mut tall = base.clone();
        tall.content = "line\n".repeat(20);
        let grown = renderer.render(&tall).unwrap();
        assert!(grown.height() > 400);
        assert!(grown.height() as f32 > tall.size * 1.3 * 19.0);
    }

    #[test]
    fn box_alignment_shifts_ink_and_paragraph_spacing_opens_gaps() {
        let mut renderer = renderer();
        let style = TextStyle {
            content: "Alpha beta gamma delta epsilon zeta eta theta".into(),
            size: 20.0,
            r#box: Some(boxed(240.0, 0.0)),
            ..Default::default()
        };
        let left = renderer.render(&style).unwrap();
        let left_ink = first_ink(&left).unwrap();
        let mut centered = style.clone();
        centered.r#box.as_mut().unwrap().align = TextAlign::Center;
        let center = renderer.render(&centered).unwrap();
        let center_ink = first_ink(&center).unwrap();
        let mut right_style = style.clone();
        right_style.r#box.as_mut().unwrap().align = TextAlign::Right;
        let right = renderer.render(&right_style).unwrap();
        let right_ink = first_ink(&right).unwrap();
        assert!(left != center && left != right && center != right);
        assert!(
            left_ink.0 < center_ink.0 && center_ink.0 < right_ink.0,
            "alignment must step the first ink rightwards: {left_ink:?} {center_ink:?} {right_ink:?}"
        );
        assert_eq!(left_ink.1, center_ink.1);
        assert_eq!(left_ink.1, right_ink.1);

        // Paragraph spacing only moves lines that follow a break.
        let mut paragraphs = style.clone();
        paragraphs.content = "First paragraph\nSecond paragraph".into();
        let tight = renderer.render(&paragraphs).unwrap();
        let mut spaced = paragraphs.clone();
        spaced.r#box.as_mut().unwrap().paragraph_spacing = 40.0;
        let loose = renderer.render(&spaced).unwrap();
        assert_eq!(tight.width(), loose.width());
        assert_eq!(loose.height(), tight.height() + 40);
        let first_row = |pixels: &RgbaImage| {
            pixels
                .enumerate_pixels()
                .find_map(|(x, y, p)| (p[3] > 0 && y > 0).then_some((x, y)))
        };
        assert_eq!(
            first_row(&tight).map(|(_, y)| y),
            first_row(&loose).map(|(_, y)| y)
        );
    }

    #[test]
    fn box_settings_are_validated_and_survive_a_round_trip() {
        let mut renderer = renderer();
        let mut style = TextStyle {
            content: "Boxed".into(),
            r#box: Some(boxed(180.0, 60.0)),
            ..Default::default()
        };
        style.r#box.as_mut().unwrap().align = TextAlign::Center;
        let layer = layer(&mut renderer, style.clone());
        let mut document = Document::new(400, 300).unwrap();
        document.insert(layer);
        let file = tempfile::NamedTempFile::new().unwrap();
        io::save(&document, file.path()).unwrap();
        let loaded = io::load(file.path()).unwrap();
        assert_eq!(loaded.active().unwrap().text, Some(style.clone()));
        assert_eq!(
            loaded.active().unwrap().pixels,
            document.active().unwrap().pixels
        );
        // A layer without a box keeps the appearance it always had.
        let mut plain = style.clone();
        plain.r#box = None;
        assert_ne!(
            plain.render_check(&mut renderer),
            style.render_check(&mut renderer)
        );

        for width in [0.0, -1.0, f32::NAN, f32::INFINITY, 30_001.0] {
            style.r#box.as_mut().unwrap().width = width;
            assert!(style.validate().is_err(), "{width}");
            assert!(renderer.render(&style).is_err(), "{width}");
        }
        style.r#box = Some(boxed(180.0, 60.0));
        for height in [-1.0, f32::NAN, 30_001.0] {
            style.r#box.as_mut().unwrap().min_height = height;
            assert!(style.validate().is_err(), "{height}");
        }
        style.r#box = Some(boxed(180.0, 60.0));
        for spacing in [0.49, 4.01, f32::NAN] {
            style.r#box.as_mut().unwrap().line_spacing = spacing;
            assert!(style.validate().is_err(), "{spacing}");
        }
        style.r#box = Some(boxed(180.0, 60.0));
        for spacing in [-1.0, 2_001.0, f32::INFINITY] {
            style.r#box.as_mut().unwrap().paragraph_spacing = spacing;
            assert!(style.validate().is_err(), "{spacing}");
        }
        style.r#box = Some(boxed(180.0, 60.0));
        assert!(style.validate().is_ok());

        // Defaults fill in whatever a stored box leaves out.
        let partial: TextStyle = serde_json::from_value(serde_json::json!({
            "content": "Partial",
            "family": "Inter Variable",
            "size": 12.0,
            "color": [0, 0, 0, 255],
            "bold": false,
            "italic": false,
            "underline": false,
            "strikethrough": false,
            "box": { "width": 300.0 }
        }))
        .unwrap();
        let text_box = partial.r#box.unwrap();
        assert_eq!(text_box.width, 300.0);
        assert_eq!(text_box.min_height, 0.0);
        assert_eq!(text_box.align, TextAlign::Left);
        assert!((text_box.line_spacing - DEFAULT_LINE_SPACING).abs() < f32::EPSILON);
        assert_eq!(text_box.paragraph_spacing, 0.0);
    }

    trait RenderCheck {
        fn render_check(&self, renderer: &mut TextRenderer) -> RgbaImage;
    }

    impl RenderCheck for TextStyle {
        fn render_check(&self, renderer: &mut TextRenderer) -> RgbaImage {
            renderer.render(self).unwrap()
        }
    }
}
