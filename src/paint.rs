use std::sync::Arc;

use anyhow::{Result, bail, ensure};
use image::{GrayImage, Luma, Rgba, RgbaImage};

use crate::{
    blend::{BlendMode, composite},
    document::{Document, Layer, Mask, Point, validate_size},
    render, selection,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintMode {
    Paint,
    Erase,
    Clone,
    Blur,
    Heal,
    Smudge,
}

#[derive(Clone, Debug)]
pub struct Brush {
    pub diameter: f32,
    pub hardness: f32,
    pub opacity: f32,
    pub smoothing: f32,
    pub color: [u8; 4],
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            diameter: 40.0,
            hardness: 0.8,
            opacity: 1.0,
            smoothing: 0.0,
            color: [0, 0, 0, 255],
        }
    }
}

pub fn smooth_stroke_point(
    anchor: Point,
    pointer: Point,
    smoothing: f32,
    zoom: f32,
) -> Option<Point> {
    let smoothing = if smoothing.is_finite() {
        smoothing.clamp(0.0, 100.0)
    } else {
        0.0
    };
    if smoothing == 0.0 {
        return Some(pointer);
    }
    if !anchor.x.is_finite()
        || !anchor.y.is_finite()
        || !pointer.x.is_finite()
        || !pointer.y.is_finite()
    {
        return Some(pointer);
    }
    let zoom = if zoom.is_finite() {
        zoom.max(0.01)
    } else {
        0.01
    };
    let radius = smoothing / zoom;
    let dx = pointer.x - anchor.x;
    let dy = pointer.y - anchor.y;
    let distance = dx.hypot(dy);
    if distance <= radius {
        return None;
    }
    let scale = (distance - radius) / distance;
    Some(Point::new(anchor.x + dx * scale, anchor.y + dy * scale))
}

pub fn ensure_pixels(layer: &mut Layer) -> Result<()> {
    ensure!(
        layer.raw.is_none(),
        "This is a RAW layer. Paint on a new pixel layer, use an adjustment layer, or rasterize it from the Layers menu."
    );
    ensure!(
        !layer.locked && !layer.group && layer.adjustment.is_none(),
        "Select an unlocked pixel layer to paint"
    );
    layer.shape = None;
    layer.text = None;
    if layer.pixels.is_none() {
        let width = layer.transform.width.round() as u32;
        let height = layer.transform.height.round() as u32;
        validate_size(width, height)?;
        layer.pixels = Some(Arc::new(RgbaImage::new(width, height)));
    }
    Ok(())
}

pub fn prepare_mask(layer: &mut Layer) -> Result<()> {
    ensure!(!layer.locked, "The layer is locked");
    let width = layer
        .pixels
        .as_ref()
        .map_or(layer.transform.width as u32, |p| p.width());
    let height = layer
        .pixels
        .as_ref()
        .map_or(layer.transform.height as u32, |p| p.height());
    validate_size(width, height)?;
    let mask = layer.mask.get_or_insert_with(Mask::white);
    if mask.pixels.dimensions() != (width, height) {
        mask.pixels = Arc::new(crate::gpu::resize_gray(&mask.pixels, width, height));
    }
    Ok(())
}

fn expand_stroke_bounds(layer: &mut Layer, from: Point, to: Point, radius: f32) -> Result<()> {
    let image = layer.pixels.as_ref().unwrap();
    let (width, height) = image.dimensions();
    let min = Point::new(from.x.min(to.x) - radius, from.y.min(to.y) - radius);
    let max = Point::new(from.x.max(to.x) + radius, from.y.max(to.y) + radius);
    let corners = [min, Point::new(max.x, min.y), max, Point::new(min.x, max.y)]
        .map(|p| layer.transform.inverse(p));
    let left =
        (corners.iter().map(|p| p.x).fold(0.0, f32::min) * width as f32 + 0.0001).floor() as i32;
    let top =
        (corners.iter().map(|p| p.y).fold(0.0, f32::min) * height as f32 + 0.0001).floor() as i32;
    let right =
        (corners.iter().map(|p| p.x).fold(1.0, f32::max) * width as f32 - 0.0001).ceil() as i32;
    let bottom =
        (corners.iter().map(|p| p.y).fold(1.0, f32::max) * height as f32 - 0.0001).ceil() as i32;
    if left == 0 && top == 0 && right == width as i32 && bottom == height as i32 {
        return Ok(());
    }
    let expanded_width = (i64::from(right) - i64::from(left)) as u32;
    let expanded_height = (i64::from(bottom) - i64::from(top)) as u32;
    validate_size(expanded_width, expanded_height)?;
    let transform = layer.transform.expanded(
        left as f32 / width as f32,
        top as f32 / height as f32,
        right as f32 / width as f32,
        bottom as f32 / height as f32,
    );
    ensure!(
        transform.valid(),
        "The expanded stroke would exceed the transform limits"
    );
    let mut expanded = RgbaImage::new(expanded_width, expanded_height);
    image::imageops::replace(&mut expanded, &**image, -i64::from(left), -i64::from(top));
    if let Some(mask) = &mut layer.mask {
        mask.placement = Some(mask.placement.unwrap_or(layer.transform));
    }
    layer.pixels = Some(Arc::new(expanded));
    layer.transform = transform;
    Ok(())
}

/// A segment covers the complete swept brush, preventing gaps at fast pointer speeds.
/// Optional source pixels are in document coordinates (used by clone and retouch tools).
pub struct StrokeOptions<'a> {
    pub mode: PaintMode,
    pub mask_target: bool,
    pub source: Option<&'a RgbaImage>,
    pub clone_offset: Point,
}

pub fn stroke(
    document: &mut Document,
    from: Point,
    to: Point,
    brush: &Brush,
    options: StrokeOptions<'_>,
) -> Result<()> {
    let StrokeOptions {
        mode,
        mask_target,
        source,
        clone_offset,
    } = options;
    let selection = document.selection.clone();
    let Some(layer) = document.active_mut() else {
        bail!("Select a layer first");
    };
    if mask_target {
        prepare_mask(layer)?;
    } else {
        ensure_pixels(layer)?;
        if matches!(mode, PaintMode::Paint | PaintMode::Clone) {
            expand_stroke_bounds(layer, from, to, (brush.diameter * 0.5).max(0.5))?;
        }
    }
    let transform = if mask_target {
        layer
            .mask
            .as_ref()
            .and_then(|m| m.placement)
            .unwrap_or(layer.transform)
    } else {
        layer.transform
    };
    let (width, height) = if mask_target {
        layer.mask.as_ref().unwrap().pixels.dimensions()
    } else {
        layer.pixels.as_ref().unwrap().dimensions()
    };
    let radius = (brush.diameter * 0.5).max(0.5);
    let min = Point::new(from.x.min(to.x) - radius, from.y.min(to.y) - radius);
    let max = Point::new(from.x.max(to.x) + radius, from.y.max(to.y) + radius);
    let local = [min, Point::new(max.x, min.y), max, Point::new(min.x, max.y)]
        .map(|p| transform.inverse(p));
    let left = (local.iter().map(|p| p.x).fold(f32::MAX, f32::min) * width as f32)
        .floor()
        .max(0.0) as u32;
    let right = (local.iter().map(|p| p.x).fold(f32::MIN, f32::max) * width as f32)
        .ceil()
        .min(width as f32) as u32;
    let top = (local.iter().map(|p| p.y).fold(f32::MAX, f32::min) * height as f32)
        .floor()
        .max(0.0) as u32;
    let bottom = (local.iter().map(|p| p.y).fold(f32::MIN, f32::max) * height as f32)
        .ceil()
        .min(height as f32) as u32;
    if crate::gpu::stroke(
        layer,
        selection.as_deref(),
        crate::gpu::Stroke {
            bounds: [left, top, right, bottom],
            endpoints: [from, to],
            brush,
            options: StrokeOptions {
                mode,
                mask_target,
                source,
                clone_offset,
            },
        },
    ) {
        return Ok(());
    }
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let length_sq = dx * dx + dy * dy;
    for y in top..bottom {
        for x in left..right {
            let point = transform.point(Point::new(
                (x as f32 + 0.5) / width as f32,
                (y as f32 + 0.5) / height as f32,
            ));
            let t = if length_sq < 0.0001 {
                0.0
            } else {
                (((point.x - from.x) * dx + (point.y - from.y) * dy) / length_sq).clamp(0.0, 1.0)
            };
            let distance = point.distance(Point::new(from.x + t * dx, from.y + t * dy)) / radius;
            if distance > 1.0 {
                continue;
            }
            let softness = if distance <= brush.hardness {
                1.0
            } else {
                (1.0 - distance) / (1.0 - brush.hardness).max(0.001)
            };
            let amount =
                softness * brush.opacity * selection::coverage(selection.as_deref(), point);
            if amount <= 0.0 {
                continue;
            }
            if mask_target {
                let pixels = Arc::make_mut(&mut layer.mask.as_mut().unwrap().pixels);
                let pixel = pixels.get_pixel_mut(x, y);
                let target = if mode == PaintMode::Erase {
                    0.0
                } else {
                    f32::from(brush.color[0]) * 0.3
                        + f32::from(brush.color[1]) * 0.59
                        + f32::from(brush.color[2]) * 0.11
                };
                pixel[0] = (f32::from(pixel[0]) * (1.0 - amount) + target * amount).round() as u8;
                continue;
            }
            let pixels = Arc::make_mut(layer.pixels.as_mut().unwrap());
            let pixel = pixels.get_pixel_mut(x, y);
            let old = pixel.0.map(|v| v as f32 / 255.0);
            let mut color = brush.color.map(|v| v as f32 / 255.0);
            match mode {
                PaintMode::Erase => {
                    pixel[3] = (old[3] * (1.0 - amount) * 255.0).round() as u8;
                    continue;
                }
                PaintMode::Clone | PaintMode::Smudge => {
                    if let Some(source) = source {
                        color = render::sample(
                            source,
                            Point::new(
                                (point.x + clone_offset.x) / source.width() as f32,
                                (point.y + clone_offset.y) / source.height() as f32,
                            ),
                        );
                    } else {
                        continue;
                    }
                }
                PaintMode::Blur | PaintMode::Heal => {
                    if let Some(source) = source {
                        color = [0.0; 4];
                        let step = if mode == PaintMode::Heal {
                            radius.max(2.0)
                        } else {
                            2.0
                        };
                        let mut weight = 0.0;
                        for sy in -1..=1 {
                            for sx in -1..=1 {
                                if mode == PaintMode::Heal && sx == 0 && sy == 0 {
                                    continue;
                                }
                                let sample = render::sample(
                                    source,
                                    Point::new(
                                        (point.x + sx as f32 * step) / source.width() as f32,
                                        (point.y + sy as f32 * step) / source.height() as f32,
                                    ),
                                );
                                // Average premultiplied samples. Near-zero alpha must
                                // not amplify roundoff into a visible retouch color.
                                for i in 0..3 {
                                    color[i] += sample[i] * sample[3];
                                }
                                weight += sample[3];
                            }
                        }
                        // Sub-byte coverage is invisible in the 8-bit source. Avoid
                        // normalizing numerical dust at fully transparent samples.
                        color = if weight < 0.5 / 255.0 {
                            [0.0; 4]
                        } else {
                            color.map(|v| v / weight)
                        };
                        // Retouching preserves alpha and interpolates the original color.
                        for i in 0..3 {
                            pixel[i] = ((old[i] * (1.0 - amount) + color[i] * amount) * 255.0)
                                .round() as u8;
                        }
                        continue;
                    } else {
                        continue;
                    }
                }
                PaintMode::Paint => {}
            }
            color[3] *= amount;
            pixel.0 = composite(old, color, BlendMode::Normal)
                .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    Ok(())
}

pub fn fill(document: &mut Document, color: [u8; 4], erase: bool, mask_target: bool) -> Result<()> {
    let selection = document.selection.clone();
    let layer = document
        .active_mut()
        .ok_or_else(|| anyhow::anyhow!("Select a layer first"))?;
    if mask_target {
        prepare_mask(layer)?;
    } else {
        ensure_pixels(layer)?;
    }
    if crate::gpu::paint(
        layer,
        selection.as_deref(),
        mask_target,
        &crate::gpu::Paint {
            mode: if erase { 1 } else { 0 },
            opacity: 1.0,
            colors: [color; 2],
            points: [Point::default(); 2],
        },
    ) {
        return Ok(());
    }
    let transform = if mask_target {
        layer
            .mask
            .as_ref()
            .and_then(|m| m.placement)
            .unwrap_or(layer.transform)
    } else {
        layer.transform
    };
    if mask_target {
        let pixels = Arc::make_mut(&mut layer.mask.as_mut().unwrap().pixels);
        let (w, h) = pixels.dimensions();
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            let point = transform.point(Point::new(
                (x as f32 + 0.5) / w as f32,
                (y as f32 + 0.5) / h as f32,
            ));
            let amount = selection::coverage(selection.as_deref(), point);
            let target = if erase { 0 } else { color[0] };
            pixel[0] = (pixel[0] as f32 * (1.0 - amount) + target as f32 * amount).round() as u8;
        }
    } else {
        let pixels = Arc::make_mut(layer.pixels.as_mut().unwrap());
        let (w, h) = pixels.dimensions();
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            let point = transform.point(Point::new(
                (x as f32 + 0.5) / w as f32,
                (y as f32 + 0.5) / h as f32,
            ));
            let amount = selection::coverage(selection.as_deref(), point);
            if erase {
                pixel[3] = (pixel[3] as f32 * (1.0 - amount)).round() as u8;
            } else {
                let mut source = color.map(|v| v as f32 / 255.0);
                source[3] *= amount;
                pixel.0 = composite(pixel.0.map(|v| v as f32 / 255.0), source, BlendMode::Normal)
                    .map(|v| (v * 255.0).round() as u8);
            }
        }
    }
    Ok(())
}

pub struct GradientOptions {
    pub foreground: [u8; 4],
    pub background: [u8; 4],
    pub radial: bool,
    pub opacity: f32,
    pub mask_target: bool,
}

pub fn gradient(
    document: &mut Document,
    start: Point,
    end: Point,
    options: GradientOptions,
) -> Result<()> {
    let GradientOptions {
        foreground,
        background,
        radial,
        opacity,
        mask_target,
    } = options;
    let selection = document.selection.clone();
    let layer = document
        .active_mut()
        .ok_or_else(|| anyhow::anyhow!("Select a layer first"))?;
    let (transform, (w, h)) = if mask_target {
        prepare_mask(layer)?;
        let mask = layer.mask.as_ref().unwrap();
        (
            mask.placement.unwrap_or(layer.transform),
            mask.pixels.dimensions(),
        )
    } else {
        ensure_pixels(layer)?;
        (layer.transform, layer.pixels.as_ref().unwrap().dimensions())
    };
    if crate::gpu::paint(
        layer,
        selection.as_deref(),
        mask_target,
        &crate::gpu::Paint {
            mode: if radial { 3 } else { 2 },
            opacity,
            colors: [foreground, background],
            points: [start, end],
        },
    ) {
        return Ok(());
    }
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_sq = (dx * dx + dy * dy).max(0.01);
    let color_at = |x, y| {
        let point = transform.point(Point::new(
            (x as f32 + 0.5) / w as f32,
            (y as f32 + 0.5) / h as f32,
        ));
        let t = if radial {
            point.distance(start) / length_sq.sqrt()
        } else {
            ((point.x - start.x) * dx + (point.y - start.y) * dy) / length_sq
        };
        let t = t.clamp(0.0, 1.0);
        let mut color: [f32; 4] = std::array::from_fn(|i| {
            (foreground[i] as f32 * (1.0 - t) + background[i] as f32 * t) / 255.0
        });
        color[3] *= opacity * selection::coverage(selection.as_deref(), point);
        color
    };
    if mask_target {
        let pixels = Arc::make_mut(&mut layer.mask.as_mut().unwrap().pixels);
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            let color = color_at(x, y);
            let target = (color[0] * 0.3 + color[1] * 0.59 + color[2] * 0.11) * 255.0;
            pixel[0] = (pixel[0] as f32 * (1.0 - color[3]) + target * color[3]).round() as u8;
        }
    } else {
        let pixels = Arc::make_mut(layer.pixels.as_mut().unwrap());
        for (x, y, pixel) in pixels.enumerate_pixels_mut() {
            pixel.0 = composite(
                pixel.0.map(|v| v as f32 / 255.0),
                color_at(x, y),
                BlendMode::Normal,
            )
            .map(|v| (v * 255.0).round() as u8);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ShapeKind {
    Rectangle,
    Ellipse,
    Line,
    RoundedRectangle,
}

pub(crate) fn line_geometry(
    size: [u32; 2],
    style: &crate::document::ShapeStyle,
) -> Result<(Point, Point, f32)> {
    ensure!(
        style.line_width.is_none_or(|width| {
            width.is_finite() && (0.0..=crate::document::MAX_SHAPE_SIZE).contains(&width)
        }),
        "Invalid line width"
    );
    let line_width = style.line_width.unwrap_or(1.0).max(1.0);
    let inset_x = line_width.min(size[0] as f32) * 0.5;
    let inset_y = line_width.min(size[1] as f32) * 0.5;
    let start = style.start.map_or(Point::new(inset_x, inset_y), |point| {
        Point::new(point.x * size[0] as f32, point.y * size[1] as f32)
    });
    let end = style.end.map_or(
        Point::new(size[0] as f32 - inset_x, size[1] as f32 - inset_y),
        |point| Point::new(point.x * size[0] as f32, point.y * size[1] as f32),
    );
    let valid = |point: Point| {
        point.x.is_finite()
            && point.y.is_finite()
            && (0.0..=size[0] as f32).contains(&point.x)
            && (0.0..=size[1] as f32).contains(&point.y)
    };
    ensure!(valid(start) && valid(end), "Invalid line endpoints");
    Ok((start, end, line_width))
}

fn rasterize_shape(size: [u32; 2], style: &crate::document::ShapeStyle) -> Result<RgbaImage> {
    validate_size(size[0], size[1])?;
    ensure!(
        style.corner_radius.is_finite() && style.corner_radius >= 0.0,
        "Invalid shape radius"
    );
    let (line_start, line_end, line_width) = if style.kind == ShapeKind::Line {
        line_geometry(size, style)?
    } else {
        (Point::default(), Point::default(), 1.0)
    };
    let radius = style.corner_radius.min(size[0].min(size[1]) as f32 * 0.5);
    let image = crate::gpu::shape(size, style).unwrap_or_else(|| {
        RgbaImage::from_fn(size[0], size[1], |x, y| {
            let mut coverage = 0.0;
            for oy in [0.25, 0.75] {
                for ox in [0.25, 0.75] {
                    let px = x as f32 + ox;
                    let py = y as f32 + oy;
                    let inside = match style.kind {
                        ShapeKind::Rectangle => true,
                        ShapeKind::Ellipse => {
                            ((px / size[0] as f32 - 0.5) * 2.0).powi(2)
                                + ((py / size[1] as f32 - 0.5) * 2.0).powi(2)
                                <= 1.0
                        }
                        ShapeKind::RoundedRectangle => {
                            let cx = px.clamp(radius, size[0] as f32 - radius);
                            let cy = py.clamp(radius, size[1] as f32 - radius);
                            (px - cx).hypot(py - cy) <= radius
                        }
                        ShapeKind::Line => {
                            let dx = line_end.x - line_start.x;
                            let dy = line_end.y - line_start.y;
                            let length_sq = dx * dx + dy * dy;
                            let t = if length_sq < 0.0001 {
                                0.0
                            } else {
                                (((px - line_start.x) * dx + (py - line_start.y) * dy) / length_sq)
                                    .clamp(0.0, 1.0)
                            };
                            Point::new(px, py)
                                .distance(Point::new(line_start.x + t * dx, line_start.y + t * dy))
                                <= line_width * 0.5
                        }
                    };
                    if inside {
                        coverage += 0.25;
                    }
                }
            }
            Rgba([
                style.color[0],
                style.color[1],
                style.color[2],
                (style.color[3] as f32 * coverage).round() as u8,
            ])
        })
    });
    Ok(image)
}

pub fn shape(
    start: Point,
    end: Point,
    kind: ShapeKind,
    color: [u8; 4],
    corner_radius: f32,
    line_width: f32,
) -> Result<Layer> {
    ensure!(
        [start.x, start.y, end.x, end.y, corner_radius, line_width]
            .iter()
            .all(|value| value.is_finite()),
        "Invalid shape geometry"
    );
    let line_width = if kind == ShapeKind::Line {
        ensure!(
            (0.0..=crate::document::MAX_SHAPE_SIZE).contains(&line_width),
            "Invalid line width"
        );
        line_width.max(1.0)
    } else {
        0.0
    };
    let (left, top, width, height) = if kind == ShapeKind::Line {
        (
            start.x.min(end.x) - line_width * 0.5,
            start.y.min(end.y) - line_width * 0.5,
            ((end.x - start.x).abs() + line_width).round().max(1.0) as u32,
            ((end.y - start.y).abs() + line_width).round().max(1.0) as u32,
        )
    } else {
        (
            start.x.min(end.x),
            start.y.min(end.y),
            (end.x - start.x).abs().round().max(1.0) as u32,
            (end.y - start.y).abs().round().max(1.0) as u32,
        )
    };
    validate_size(width, height)?;
    let transform_width = width as f32;
    let transform_height = height as f32;
    let style = crate::document::ShapeStyle {
        kind,
        color,
        corner_radius,
        line_width: (kind == ShapeKind::Line).then_some(line_width),
        start: (kind == ShapeKind::Line).then(|| {
            Point::new(
                (start.x - left) / transform_width,
                (start.y - top) / transform_height,
            )
        }),
        end: (kind == ShapeKind::Line).then(|| {
            Point::new(
                (end.x - left) / transform_width,
                (end.y - top) / transform_height,
            )
        }),
    };
    let image = rasterize_shape([width, height], &style)?;
    let mut layer = Layer::image(
        match kind {
            ShapeKind::Rectangle => "Rectangle",
            ShapeKind::Ellipse => "Ellipse",
            ShapeKind::Line => "Line",
            ShapeKind::RoundedRectangle => "Rounded rectangle",
        },
        image,
    );
    layer.shape = Some(style);
    layer.transform.x = left;
    layer.transform.y = top;
    Ok(layer)
}

pub fn refresh_shapes(document: &mut Document) -> Result<()> {
    for layer in &mut document.layers {
        let Some(style) = &layer.shape else { continue };
        let corners = layer.transform.corners();
        let width = corners[0].distance(corners[1]).round().max(1.0) as u32;
        let height = corners[0].distance(corners[3]).round().max(1.0) as u32;
        if layer
            .pixels
            .as_ref()
            .is_some_and(|p| p.dimensions() == (width, height))
        {
            continue;
        }
        layer.pixels = Some(Arc::new(rasterize_shape([width, height], style)?));
    }
    Ok(())
}

pub fn mask_from_selection(document: &Document, layer: &Layer) -> GrayImage {
    let (w, h) = layer
        .pixels
        .as_ref()
        .map_or((document.width, document.height), |p| p.dimensions());
    if let Some(result) =
        crate::gpu::project_selection([w, h], layer.transform, document.selection.as_deref())
    {
        return result;
    }
    GrayImage::from_fn(w, h, |x, y| {
        let point = layer.transform.point(Point::new(
            (x as f32 + 0.5) / w as f32,
            (y as f32 + 0.5) / h as f32,
        ));
        Luma([(selection::coverage(document.selection.as_deref(), point) * 255.0).round() as u8])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_gradient_respects_placement_selection_opacity_and_color_alpha() {
        let mut doc = Document::new(12, 8).unwrap();
        let mut layer = shape(
            Point::default(),
            Point::new(4.0, 2.0),
            ShapeKind::Rectangle,
            [50, 100, 150, 255],
            0.0,
            0.0,
        )
        .unwrap();
        layer.mask = Some(Mask {
            linked: false,
            placement: Some(crate::document::Transform {
                x: 4.0,
                y: 2.0,
                ..crate::document::Transform::new(8, 4)
            }),
            ..Mask::white()
        });
        let before = layer.clone();
        doc.insert(layer);
        let mut selection = GrayImage::new(12, 8);
        selection.put_pixel(5, 3, Luma([128]));
        selection.put_pixel(7, 3, Luma([255]));
        selection.put_pixel(11, 3, Luma([255]));
        doc.selection = Some(Arc::new(selection));

        gradient(
            &mut doc,
            Point::new(5.0, 3.0),
            Point::new(11.0, 3.0),
            GradientOptions {
                foreground: [255, 0, 0, 128],
                background: [0, 0, 255, 0],
                radial: false,
                opacity: 0.5,
                mask_target: true,
            },
        )
        .unwrap();

        let layer = doc.active().unwrap();
        assert!(Arc::ptr_eq(
            layer.pixels.as_ref().unwrap(),
            before.pixels.as_ref().unwrap(),
        ));
        assert!(layer.shape.is_some());
        let mask = layer.mask.as_ref().unwrap();
        assert_eq!(mask.placement, before.mask.as_ref().unwrap().placement);
        assert!(!mask.linked);
        assert_eq!(mask.pixels.dimensions(), (4, 2));
        assert_eq!(
            mask.pixels.as_raw(),
            &[233, 222, 255, 255, 255, 255, 255, 255]
        );
        assert_eq!(before.mask.unwrap().pixels.as_raw(), &[255]);
    }

    #[test]
    fn mask_gradient_supports_non_pixel_layers_and_rejects_locked_layers() {
        for group in [false, true] {
            let mut doc = Document::new(4, 2).unwrap();
            let layer = doc.active_mut().unwrap();
            layer.group = group;
            layer.adjustment = (!group).then_some(crate::document::Adjustment::Invert);
            layer.mask = Some(Mask::white());
            let options = || GradientOptions {
                foreground: [0, 0, 0, 255],
                background: [255; 4],
                radial: false,
                opacity: 1.0,
                mask_target: true,
            };
            let start = Point::new(0.5, 0.5);
            let end = Point::new(3.5, 0.5);

            gradient(&mut doc, start, end, options()).unwrap();
            let layer = doc.active_mut().unwrap();
            assert!(layer.pixels.is_none());
            assert_eq!(layer.group, group);
            assert_eq!(
                layer.adjustment,
                (!group).then_some(crate::document::Adjustment::Invert),
            );
            let pixels = layer.mask.as_ref().unwrap().pixels.clone();
            assert_eq!(pixels.as_raw(), &[0, 85, 170, 255, 0, 85, 170, 255]);

            layer.locked = true;
            assert!(gradient(&mut doc, start, end, options()).is_err());
            assert!(Arc::ptr_eq(
                &pixels,
                &doc.active().unwrap().mask.as_ref().unwrap().pixels,
            ));
        }
    }

    #[test]
    fn brush_extends_an_imported_layer_without_moving_existing_pixels() {
        let mut doc = Document::new(20, 20).unwrap();
        let mut layer = Layer::image("small", RgbaImage::from_pixel(4, 4, Rgba([0, 0, 255, 255])));
        layer.transform.x = 8.0;
        layer.transform.y = 8.0;
        doc.insert(layer);
        stroke(
            &mut doc,
            Point::new(13.0, 10.0),
            Point::new(16.0, 10.0),
            &Brush {
                diameter: 2.0,
                hardness: 1.0,
                color: [255, 0, 0, 255],
                ..Brush::default()
            },
            StrokeOptions {
                mode: PaintMode::Paint,
                mask_target: false,
                source: None,
                clone_offset: Point::default(),
            },
        )
        .unwrap();
        let pixels = render::render(&doc);
        assert_eq!(pixels.get_pixel(9, 9).0, [0, 0, 255, 255]);
        assert_eq!(pixels.get_pixel(14, 10).0, [255, 0, 0, 255]);
        doc.validate().unwrap();
    }

    #[test]
    fn live_shapes_redraw_at_new_sizes_until_their_pixels_are_edited() {
        let mut doc = Document::new(64, 64).unwrap();
        let layer = shape(
            Point::default(),
            Point::new(20.0, 20.0),
            ShapeKind::RoundedRectangle,
            [255, 0, 0, 255],
            5.0,
            0.0,
        )
        .unwrap();
        doc.insert(layer);
        doc.active_mut().unwrap().transform.width = 40.0;
        refresh_shapes(&mut doc).unwrap();
        let layer = doc.active().unwrap();
        assert_eq!(layer.pixels.as_ref().unwrap().dimensions(), (40, 20));
        assert_eq!(layer.pixels.as_ref().unwrap().get_pixel(6, 0)[3], 255);
        fill(&mut doc, [0, 0, 255, 255], false, false).unwrap();
        assert!(doc.active().unwrap().shape.is_none());
    }

    #[test]
    fn line_shape_rasterizes_with_its_own_width_and_endpoints() {
        let layer = shape(
            Point::new(4.0, 12.0),
            Point::new(20.0, 12.0),
            ShapeKind::Line,
            [255, 0, 0, 255],
            0.0,
            4.0,
        )
        .unwrap();
        let style = layer.shape.as_ref().unwrap();
        assert_eq!(style.line_width, Some(4.0));
        assert!((style.start.unwrap().x - 0.1).abs() < 0.00001);
        assert!((style.start.unwrap().y - 0.5).abs() < 0.00001);
        assert!((style.end.unwrap().x - 0.9).abs() < 0.00001);
        assert_eq!(style.end.unwrap().y, 0.5);
        let pixels = layer.pixels.as_ref().unwrap();
        assert_eq!(pixels.dimensions(), (20, 4));
        assert_eq!(pixels.get_pixel(10, 1).0, [255, 0, 0, 255]);
        assert_eq!(pixels.get_pixel(10, 2).0, [255, 0, 0, 255]);
        assert!(pixels.get_pixel(0, 0)[3] < 255);
    }

    #[test]
    fn live_line_refreshes_at_a_new_size_without_scaling_its_width() {
        let mut doc = Document::new(64, 32).unwrap();
        doc.insert(
            shape(
                Point::new(4.0, 8.0),
                Point::new(20.0, 8.0),
                ShapeKind::Line,
                [255, 0, 0, 255],
                0.0,
                3.0,
            )
            .unwrap(),
        );
        let original_style = doc.active().unwrap().shape.clone().unwrap();
        let layer = doc.active_mut().unwrap();
        layer.transform.width *= 2.0;
        layer.transform.height *= 2.0;
        refresh_shapes(&mut doc).unwrap();
        let layer = doc.active().unwrap();
        assert_eq!(layer.pixels.as_ref().unwrap().dimensions(), (38, 6));
        assert_eq!(layer.shape.as_ref().unwrap(), &original_style);
        let pixels = layer.pixels.as_ref().unwrap();
        assert_eq!(pixels.get_pixel(19, 2).0, [255, 0, 0, 255]);
        assert_eq!(pixels.get_pixel(19, 0)[3], 0);
        assert_eq!(render::render(&doc).get_pixel(21, 9).0, [255, 0, 0, 255]);
    }

    #[test]
    fn stroke_smoothing_uses_a_screen_space_string_and_ignores_slack() {
        let anchor = Point::new(10.0, 20.0);
        let pointer = Point::new(13.0, 24.0);
        assert_eq!(smooth_stroke_point(anchor, pointer, 5.0, 1.0), None);
        let painted = smooth_stroke_point(anchor, pointer, 2.0, 1.0).unwrap();
        assert!((painted.x - 11.8).abs() < 0.00001);
        assert!((painted.y - 22.4).abs() < 0.00001);
        let zoomed = smooth_stroke_point(anchor, pointer, 2.0, 2.0).unwrap();
        assert!((zoomed.x - 12.4).abs() < 0.00001);
        assert!((zoomed.y - 23.2).abs() < 0.00001);
        assert_eq!(
            smooth_stroke_point(anchor, pointer, 0.0, 2.0),
            Some(pointer)
        );
    }

    #[test]
    fn fast_stroke_has_no_gaps_and_respects_selection() {
        let mut doc = Document::new(30, 10).unwrap();
        doc.selection = Some(Arc::new(selection::rectangle(
            30,
            10,
            Point::new(0.0, 0.0),
            Point::new(15.0, 10.0),
            false,
        )));
        let brush = Brush {
            diameter: 4.0,
            hardness: 1.0,
            color: [255, 0, 0, 255],
            ..Brush::default()
        };
        stroke(
            &mut doc,
            Point::new(1.0, 5.0),
            Point::new(28.0, 5.0),
            &brush,
            StrokeOptions {
                mode: PaintMode::Paint,
                mask_target: false,
                source: None,
                clone_offset: Point::default(),
            },
        )
        .unwrap();
        let pixels = render::render(&doc);
        for x in 1..15 {
            assert_eq!(pixels.get_pixel(x, 5)[3], 255);
        }
        for x in 15..30 {
            assert_eq!(pixels.get_pixel(x, 5)[3], 0);
        }
    }

    #[test]
    fn painting_preserves_shared_history_pixels() {
        let mut doc = Document::new(8, 8).unwrap();
        fill(&mut doc, [255, 0, 0, 255], false, false).unwrap();
        let before = doc.clone();
        fill(&mut doc, [0, 0, 255, 255], false, false).unwrap();
        assert_eq!(
            before
                .active()
                .unwrap()
                .pixels
                .as_ref()
                .unwrap()
                .get_pixel(0, 0)
                .0,
            [255, 0, 0, 255]
        );
        assert_eq!(
            doc.active()
                .unwrap()
                .pixels
                .as_ref()
                .unwrap()
                .get_pixel(0, 0)
                .0,
            [0, 0, 255, 255]
        );
    }
}
