use super::widgets;
use std::sync::Arc;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use mectov::{
    document::{GridSettings, Guide, GuideAxis, MAX_GUIDES, Point},
    operations,
    paint::{self, PaintMode},
    render,
    selection::{self, SelectionMode},
};

use super::{
    EditorApp, Gesture, GuideDrag, GuideEdit, SnapLine, SnapSource, Tool, TransformDrag, theme,
};

const HANDLES: [Point; 8] = [
    Point::new(0.0, 0.0),
    Point::new(0.5, 0.0),
    Point::new(1.0, 0.0),
    Point::new(1.0, 0.5),
    Point::new(1.0, 1.0),
    Point::new(0.5, 1.0),
    Point::new(0.0, 1.0),
    Point::new(0.0, 0.5),
];

const MAX_SMOOTHING_HISTORY: usize = 8;
const RULER_SIZE: f32 = 20.0;
const GUIDE_GRAB_PX: f32 = 6.0;

fn nice_step(target: f32) -> f32 {
    if !target.is_finite() || target <= 0.0 {
        return 1.0;
    }
    let exponent = target.log10().floor().clamp(-6.0, 6.0) as i32;
    let base = 10.0_f32.powi(exponent);
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|factor| factor * base)
        .find(|step| *step >= target)
        .unwrap_or(10.0 * base)
}

fn effective_grid_step(grid: GridSettings, zoom: f32) -> Option<f32> {
    let mut step = grid.minor_spacing();
    if !step.is_finite() || step <= 0.0 || !zoom.is_finite() || zoom <= 0.0 {
        return None;
    }
    while step * zoom < 2.0 {
        step *= 10.0;
        if !step.is_finite() {
            return None;
        }
    }
    Some(step)
}

fn line_values(min: f32, max: f32, step: f32) -> Vec<f32> {
    if !min.is_finite() || !max.is_finite() || !step.is_finite() || step <= 0.0 || max < min {
        return Vec::new();
    }
    let first = (min / step).ceil() * step;
    let count = ((max - first) / step).floor().max(0.0) as usize;
    (0..=count.min(512))
        .map(|index| first + index as f32 * step)
        .filter(|value| *value >= min && *value <= max)
        .collect()
}

fn constrain_shape_point(start: Point, point: Point, kind: mectov::paint::ShapeKind) -> Point {
    if kind == mectov::paint::ShapeKind::Line {
        let dx = point.x - start.x;
        let dy = point.y - start.y;
        let length = dx.hypot(dy);
        if length <= f32::EPSILON {
            return point;
        }
        let angle =
            (dy.atan2(dx) / (std::f32::consts::PI / 4.0)).round() * (std::f32::consts::PI / 4.0);
        return Point::new(
            start.x + angle.cos() * length,
            start.y + angle.sin() * length,
        );
    }
    let dx = point.x - start.x;
    let dy = point.y - start.y;
    let size = dx.abs().max(dy.abs());
    Point::new(start.x + size * dx.signum(), start.y + size * dy.signum())
}

fn constrain_crop_point(start: Point, point: Point, ratio: f32) -> Point {
    if !ratio.is_finite() || ratio <= 0.0 {
        return point;
    }
    let dx = point.x - start.x;
    let dy = point.y - start.y;
    if dx == 0.0 && dy == 0.0 {
        return point;
    }
    let sign_x = if dx < 0.0 { -1.0 } else { 1.0 };
    let sign_y = if dy < 0.0 { -1.0 } else { 1.0 };
    let (width, height) = if dx.abs() >= dy.abs() {
        (dx.abs(), dx.abs() / ratio)
    } else {
        (dy.abs() * ratio, dy.abs())
    };
    Point::new(start.x + sign_x * width, start.y + sign_y * height)
}

fn record_smoothing_point(points: &mut Vec<Point>, point: Point) {
    if points.last().copied() == Some(point) {
        return;
    }
    if points.len() >= MAX_SMOOTHING_HISTORY {
        let excess = points.len() - MAX_SMOOTHING_HISTORY + 1;
        points.drain(..excess);
    }
    points.push(point);
}

fn drag_transform(
    old: mectov::document::Transform,
    start: Point,
    point: Point,
    kind: TransformDrag,
    lock_ratio: bool,
    shift: bool,
) -> mectov::document::Transform {
    let mut t = old;
    match kind {
        TransformDrag::Move | TransformDrag::Pixels => {
            t.x += point.x - start.x;
            t.y += point.y - start.y;
        }
        TransformDrag::Rotate => {
            let center = old.center();
            let a = (start.y - center.y).atan2(start.x - center.x);
            let b = (point.y - center.y).atan2(point.x - center.x);
            let angle = (b - a).to_degrees();
            t.rotation += if shift {
                (angle / 15.0).round() * 15.0
            } else {
                angle
            };
        }
        TransformDrag::Scale(index) => {
            let handle = HANDLES[index];
            let anchor = Point::new(1.0 - handle.x, 1.0 - handle.y);
            let unit = old.inverse(point);
            let anchor_point = old.point(anchor);
            let mut sx = if handle.x == 0.5 {
                1.0
            } else {
                ((unit.x - anchor.x) / (handle.x - anchor.x)).max(1.0 / old.width)
            };
            let mut sy = if handle.y == 0.5 {
                1.0
            } else {
                ((unit.y - anchor.y) / (handle.y - anchor.y)).max(1.0 / old.height)
            };
            if lock_ratio != shift {
                let factor = if handle.x == 0.5 {
                    sy
                } else if handle.y == 0.5 {
                    sx
                } else {
                    sx.max(sy)
                };
                sx = factor;
                sy = factor;
            }
            t.width = (old.width * sx).clamp(1.0, 300_000.0);
            t.height = (old.height * sy).clamp(1.0, 300_000.0);
            let moved_anchor = t.point(anchor);
            t.x += anchor_point.x - moved_anchor.x;
            t.y += anchor_point.y - moved_anchor.y;
        }
        TransformDrag::Distort(index) => {
            let mut affine = old;
            affine.warp = None;
            let mut quad = old
                .warp
                .unwrap_or([HANDLES[0], HANDLES[2], HANDLES[4], HANDLES[6]]);
            let mut moved = point;
            if shift {
                if (point.x - start.x).abs() > (point.y - start.y).abs() {
                    moved.y = start.y;
                } else {
                    moved.x = start.x;
                }
            }
            quad[index] = affine.inverse(moved);
            if mectov::geometry::Homography::from_quad(quad).is_some() {
                t.warp = Some(quad);
            }
        }
        TransformDrag::Selection => {}
    }
    t
}

impl EditorApp {
    pub(super) fn canvas(&mut self, ctx: &egui::Context) {
        let mut pending_guide: Option<GuideEdit> = None;
        let mut ruler_rects = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::CANVAS))
            .show(ctx, |ui| {
                let show_rulers = self.show_rulers;
                let (viewport, allocated) = ui.allocate_exact_size(
                    ui.available_size(),
                    if show_rulers {
                        Sense::hover()
                    } else {
                        Sense::click_and_drag()
                    },
                );
                let (canvas_area, horizontal_ruler, vertical_ruler) = if show_rulers {
                    let horizontal =
                        Rect::from_min_size(viewport.min, vec2(viewport.width(), RULER_SIZE));
                    let vertical = Rect::from_min_max(
                        pos2(viewport.left(), viewport.top() + RULER_SIZE),
                        pos2(viewport.left() + RULER_SIZE, viewport.bottom()),
                    );
                    let area = Rect::from_min_max(
                        pos2(viewport.left() + RULER_SIZE, viewport.top() + RULER_SIZE),
                        viewport.max,
                    );
                    ruler_rects = Some([horizontal, vertical]);
                    (area, horizontal, vertical)
                } else {
                    (viewport, Rect::NOTHING, Rect::NOTHING)
                };
                let response = if show_rulers {
                    ui.interact(canvas_area, ui.id().with("canvas"), Sense::click_and_drag())
                } else {
                    allocated
                };
                self.ruler_rects = ruler_rects;
                if self.sessions.is_empty() {
                    self.welcome(ui, viewport);
                    return;
                }
                let session = &mut self.sessions[self.current];
                if session.fit {
                    session.zoom = ((canvas_area.width() - 100.0) / session.document.width as f32)
                        .min((canvas_area.height() - 90.0) / session.document.height as f32)
                        .clamp(0.01, 8.0);
                    session.pan = Vec2::ZERO;
                    session.fit = false;
                }
                session.refresh(ctx, self.gpu_state.as_ref());
                let zoom = session.zoom;
                let size = vec2(
                    session.document.width as f32,
                    session.document.height as f32,
                ) * zoom;
                let origin = canvas_area.center() - size * 0.5 + session.pan;
                let canvas = Rect::from_min_size(origin, size);
                self.canvas_rect = Some(canvas);
                let visible = canvas.intersect(canvas_area);
                let painter = ui.painter().with_clip_rect(canvas_area);
                if show_rulers {
                    let horizontal_rect =
                        Rect::from_min_size(viewport.min, vec2(viewport.width(), RULER_SIZE));
                    let vertical_rect = Rect::from_min_max(
                        pos2(viewport.left(), viewport.top() + RULER_SIZE),
                        pos2(viewport.left() + RULER_SIZE, viewport.bottom()),
                    );
                    let horizontal = ui.painter().with_clip_rect(horizontal_rect);
                    let vertical = ui.painter().with_clip_rect(vertical_rect);
                    horizontal.rect_filled(horizontal_rect, 0.0, theme::PANEL);
                    vertical.rect_filled(vertical_rect, 0.0, theme::PANEL);
                    let step = nice_step(60.0 / zoom);
                    let min_x = (viewport.left() - origin.x) / zoom;
                    let max_x = (viewport.right() - origin.x) / zoom;
                    let min_y = (viewport.top() + RULER_SIZE - origin.y) / zoom;
                    let max_y = (viewport.bottom() - origin.y) / zoom;
                    for x in line_values(min_x, max_x, step / 5.0) {
                        let screen = origin.x + x * zoom;
                        horizontal.line_segment(
                            [
                                pos2(screen, horizontal_rect.bottom()),
                                pos2(screen, horizontal_rect.bottom() - 4.0),
                            ],
                            Stroke::new(0.5_f32, theme::MUTED),
                        );
                    }
                    for y in line_values(min_y, max_y, step / 5.0) {
                        let screen = origin.y + y * zoom;
                        vertical.line_segment(
                            [
                                pos2(vertical_rect.right() - 4.0, screen),
                                pos2(vertical_rect.right(), screen),
                            ],
                            Stroke::new(0.5_f32, theme::MUTED),
                        );
                    }
                    for x in line_values(min_x, max_x, step) {
                        let screen = origin.x + x * zoom;
                        horizontal.line_segment(
                            [
                                pos2(screen, horizontal_rect.bottom()),
                                pos2(screen, horizontal_rect.top() + 8.0),
                            ],
                            Stroke::new(0.75_f32, theme::TEXT),
                        );
                        horizontal.text(
                            pos2(screen + 2.0, horizontal_rect.center().y),
                            Align2::LEFT_CENTER,
                            format!("{x:.0}"),
                            FontId::proportional(9.0),
                            theme::MUTED,
                        );
                    }
                    for y in line_values(min_y, max_y, step) {
                        let screen = origin.y + y * zoom;
                        vertical.line_segment(
                            [
                                pos2(vertical_rect.left(), screen),
                                pos2(vertical_rect.left() + 4.0, screen),
                            ],
                            Stroke::new(0.75_f32, theme::TEXT),
                        );
                        vertical.text(
                            pos2(vertical_rect.left() + 6.0, screen),
                            Align2::LEFT_CENTER,
                            format!("{y:.0}"),
                            FontId::proportional(9.0),
                            theme::MUTED,
                        );
                    }
                    let guide_color = Color32::from_rgb(0, 255, 255);
                    for guide in &session.document.guides {
                        match guide.axis {
                            GuideAxis::Vertical => {
                                let screen = origin.x + guide.position * zoom;
                                if (horizontal_rect.left()..=horizontal_rect.right())
                                    .contains(&screen)
                                {
                                    horizontal.line_segment(
                                        [
                                            pos2(screen - 4.0, horizontal_rect.bottom()),
                                            pos2(screen + 4.0, horizontal_rect.bottom()),
                                        ],
                                        Stroke::new(1.5_f32, guide_color),
                                    );
                                }
                            }
                            GuideAxis::Horizontal => {
                                let screen = origin.y + guide.position * zoom;
                                if (vertical_rect.top()..=vertical_rect.bottom()).contains(&screen)
                                {
                                    vertical.line_segment(
                                        [
                                            pos2(vertical_rect.left(), screen - 4.0),
                                            pos2(vertical_rect.left(), screen + 4.0),
                                        ],
                                        Stroke::new(1.5_f32, guide_color),
                                    );
                                }
                            }
                        }
                    }
                    if let Some(pointer) = ctx.input(|input| input.pointer.hover_pos())
                        && canvas_area.contains(pointer)
                    {
                        let x = (pointer.x - origin.x) / zoom;
                        let y = (pointer.y - origin.y) / zoom;
                        let chip = Rect::from_min_size(
                            pos2(viewport.left() + 1.0, viewport.top() + RULER_SIZE - 13.0),
                            vec2(48.0, 12.0),
                        );
                        horizontal.rect_filled(chip, 0.0, theme::TITLEBAR);
                        horizontal.text(
                            pos2(pointer.x - 2.0, viewport.top() + RULER_SIZE - 2.0),
                            Align2::RIGHT_BOTTOM,
                            format!("{x:.1}"),
                            FontId::proportional(9.0),
                            theme::TEXT,
                        );
                        let chip = Rect::from_center_size(
                            pos2(viewport.left() + 10.0, pointer.y),
                            vec2(18.0, 12.0),
                        );
                        vertical.rect_filled(chip, 0.0, theme::TITLEBAR);
                        vertical.text(
                            pos2(viewport.left() + 2.0, pointer.y),
                            Align2::LEFT_CENTER,
                            format!("{y:.1}"),
                            FontId::proportional(9.0),
                            theme::TEXT,
                        );
                    }
                }
                painter.rect_filled(canvas.expand(3.0), 0.0, Color32::from_black_alpha(60));
                if visible.is_positive() {
                    let checker = 12.0;
                    let min_x = ((visible.left() - origin.x) / checker).floor() as i32;
                    let max_x = ((visible.right() - origin.x) / checker).ceil() as i32;
                    let min_y = ((visible.top() - origin.y) / checker).floor() as i32;
                    let max_y = ((visible.bottom() - origin.y) / checker).ceil() as i32;
                    let checker_painter = painter.with_clip_rect(visible);
                    for y in min_y..max_y {
                        for x in min_x..max_x {
                            checker_painter.rect_filled(
                                Rect::from_min_size(
                                    origin + vec2(x as f32 * checker, y as f32 * checker),
                                    Vec2::splat(checker),
                                ),
                                0.0,
                                Color32::from_gray(if (x + y) % 2 == 0 { 66 } else { 80 }),
                            );
                        }
                    }
                    if let Some(texture) = session
                        .gpu
                        .as_ref()
                        .and_then(|g| g.texture)
                        .or_else(|| session.texture.as_ref().map(|t| t.id()))
                    {
                        painter.image(
                            texture,
                            canvas,
                            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                }
                painter.rect_stroke(
                    canvas,
                    0.0,
                    Stroke::new(1.0_f32, Color32::from_gray(17)),
                    StrokeKind::Outside,
                );
                let map = |p: Point| origin + vec2(p.x, p.y) * zoom;
                if zoom >= 8.0 {
                    let start = ((visible.left() - origin.x) / zoom).floor().max(0.0) as u32;
                    let end = ((visible.right() - origin.x) / zoom)
                        .ceil()
                        .min(session.document.width as f32) as u32;
                    for x in start..=end {
                        painter.line_segment(
                            [
                                pos2(origin.x + x as f32 * zoom, visible.top()),
                                pos2(origin.x + x as f32 * zoom, visible.bottom()),
                            ],
                            Stroke::new(0.5_f32, Color32::from_white_alpha(28)),
                        );
                    }
                    let start = ((visible.top() - origin.y) / zoom).floor().max(0.0) as u32;
                    let end = ((visible.bottom() - origin.y) / zoom)
                        .ceil()
                        .min(session.document.height as f32) as u32;
                    for y in start..=end {
                        painter.line_segment(
                            [
                                pos2(visible.left(), origin.y + y as f32 * zoom),
                                pos2(visible.right(), origin.y + y as f32 * zoom),
                            ],
                            Stroke::new(0.5_f32, Color32::from_white_alpha(28)),
                        );
                    }
                }
                if self.show_grid
                    && let Some(grid) = session.document.grid
                    && let Some(minor_step) = effective_grid_step(grid, zoom)
                {
                    let min_x = (visible.left() - origin.x) / zoom;
                    let max_x = (visible.right() - origin.x) / zoom;
                    let min_y = (visible.top() - origin.y) / zoom;
                    let max_y = (visible.bottom() - origin.y) / zoom;
                    let minor =
                        Stroke::new(0.5_f32, Color32::from_rgba_unmultiplied(128, 128, 128, 46));
                    let major =
                        Stroke::new(0.75_f32, Color32::from_rgba_unmultiplied(160, 160, 160, 85));
                    let grid_painter = painter.with_clip_rect(visible);
                    for x in line_values(min_x, max_x, minor_step) {
                        let screen = origin.x + x * zoom;
                        grid_painter.line_segment(
                            [pos2(screen, visible.top()), pos2(screen, visible.bottom())],
                            minor,
                        );
                    }
                    for y in line_values(min_y, max_y, minor_step) {
                        let screen = origin.y + y * zoom;
                        grid_painter.line_segment(
                            [pos2(visible.left(), screen), pos2(visible.right(), screen)],
                            minor,
                        );
                    }
                    if (minor_step - grid.spacing).abs() > f32::EPSILON {
                        for x in line_values(min_x, max_x, grid.spacing) {
                            let screen = origin.x + x * zoom;
                            grid_painter.line_segment(
                                [pos2(screen, visible.top()), pos2(screen, visible.bottom())],
                                major,
                            );
                        }
                        for y in line_values(min_y, max_y, grid.spacing) {
                            let screen = origin.y + y * zoom;
                            grid_painter.line_segment(
                                [pos2(visible.left(), screen), pos2(visible.right(), screen)],
                                major,
                            );
                        }
                    }
                }
                if let Some(mask) = &session.document.selection {
                    let step = (1.0 / zoom).ceil().max(1.0) as usize;
                    let start_x = ((visible.left() - origin.x) / zoom).floor().max(0.0) as u32;
                    let start_y = ((visible.top() - origin.y) / zoom).floor().max(0.0) as u32;
                    let end_x = ((visible.right() - origin.x) / zoom)
                        .ceil()
                        .min(mask.width() as f32) as u32;
                    let end_y = ((visible.bottom() - origin.y) / zoom)
                        .ceil()
                        .min(mask.height() as f32) as u32;
                    for y in (start_y..end_y).step_by(step) {
                        for x in (start_x..end_x).step_by(step) {
                            if mask.get_pixel(x, y)[0] < 128 {
                                continue;
                            }
                            let s = step as u32;
                            let color = if (x + y) / s % 8 < 4 {
                                Color32::WHITE
                            } else {
                                Color32::BLACK
                            };
                            if y < s || mask.get_pixel(x, y - s)[0] < 128 {
                                painter.line_segment(
                                    [
                                        map(Point::new(x as f32, y as f32)),
                                        map(Point::new((x + s) as f32, y as f32)),
                                    ],
                                    Stroke::new(1.0_f32, color),
                                );
                            }
                            if x < s || mask.get_pixel(x - s, y)[0] < 128 {
                                painter.line_segment(
                                    [
                                        map(Point::new(x as f32, y as f32)),
                                        map(Point::new(x as f32, (y + s) as f32)),
                                    ],
                                    Stroke::new(1.0_f32, color),
                                );
                            }
                            if y + s >= mask.height() || mask.get_pixel(x, y + s)[0] < 128 {
                                painter.line_segment(
                                    [
                                        map(Point::new(x as f32, (y + s) as f32)),
                                        map(Point::new((x + s) as f32, (y + s) as f32)),
                                    ],
                                    Stroke::new(1.0_f32, color),
                                );
                            }
                            if x + s >= mask.width() || mask.get_pixel(x + s, y)[0] < 128 {
                                painter.line_segment(
                                    [
                                        map(Point::new((x + s) as f32, y as f32)),
                                        map(Point::new((x + s) as f32, (y + s) as f32)),
                                    ],
                                    Stroke::new(1.0_f32, color),
                                );
                            }
                        }
                    }
                }
                let mut hover_handle = None;
                if self.tool == Tool::Move
                    && self.show_controls
                    && let Some(t) = operations::transform_box(&session.document, self.mask_target)
                {
                    let corners = t.corners().map(map);
                    painter.add(egui::Shape::closed_line(
                        corners.to_vec(),
                        Stroke::new(1.0_f32, Color32::from_gray(225)),
                    ));
                    for (index, unit) in HANDLES.iter().enumerate() {
                        let point = map(t.point(*unit));
                        painter.rect(
                            Rect::from_center_size(point, Vec2::splat(6.0)),
                            0.0,
                            Color32::from_gray(245),
                            Stroke::new(1.0_f32, Color32::from_gray(55)),
                            StrokeKind::Outside,
                        );
                        if response
                            .hover_pos()
                            .is_some_and(|p| p.distance(point) < 8.0)
                        {
                            hover_handle = Some(TransformDrag::Scale(index));
                        }
                    }
                    let top = map(t.point(Point::new(0.5, 0.0)));
                    let center = map(t.center());
                    let rotate = top + (top - center).normalized() * 23.0;
                    painter.line_segment([top, rotate], Stroke::new(1.0_f32, theme::TEXT));
                    painter.circle_filled(rotate, 3.5, theme::TEXT);
                    if response
                        .hover_pos()
                        .is_some_and(|p| p.distance(rotate) < 8.0)
                    {
                        hover_handle = Some(TransformDrag::Rotate);
                    }
                }
                // Guides the document carries. They span the canvas rather than the viewport, so one pushed
                // off the canvas edge stops at it.
                let guide_painter = painter.with_clip_rect(canvas.intersect(viewport));
                for (index, guide) in session.document.guides.iter().enumerate() {
                    if self
                        .guide_drag
                        .is_some_and(|drag| drag.existing == Some(index))
                    {
                        continue;
                    }
                    let line = match guide.axis {
                        GuideAxis::Horizontal => [
                            pos2(canvas.left(), origin.y + guide.position * zoom),
                            pos2(canvas.right(), origin.y + guide.position * zoom),
                        ],
                        GuideAxis::Vertical => [
                            pos2(origin.x + guide.position * zoom, canvas.top()),
                            pos2(origin.x + guide.position * zoom, canvas.bottom()),
                        ],
                    };
                    guide_painter.line_segment(
                        line,
                        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(0, 255, 255, 230)),
                    );
                }
                if let Some(drag) = self.guide_drag {
                    let line = match drag.axis {
                        GuideAxis::Horizontal => [
                            pos2(canvas.left(), origin.y + drag.position * zoom),
                            pos2(canvas.right(), origin.y + drag.position * zoom),
                        ],
                        GuideAxis::Vertical => [
                            pos2(origin.x + drag.position * zoom, canvas.top()),
                            pos2(origin.x + drag.position * zoom, canvas.bottom()),
                        ],
                    };
                    guide_painter.line_segment(
                        line,
                        Stroke::new(1.5_f32, Color32::from_rgba_unmultiplied(0, 255, 255, 255)),
                    );
                }
                for indicator in &self.snap_indicators {
                    let line = match indicator.axis {
                        GuideAxis::Horizontal => [
                            pos2(viewport.left(), origin.y + indicator.position * zoom),
                            pos2(viewport.right(), origin.y + indicator.position * zoom),
                        ],
                        GuideAxis::Vertical => [
                            pos2(origin.x + indicator.position * zoom, viewport.top()),
                            pos2(origin.x + indicator.position * zoom, viewport.bottom()),
                        ],
                    };
                    let color = match indicator.source {
                        super::SnapSource::Guide | super::SnapSource::Grid => {
                            Color32::from_rgb(0, 255, 255)
                        }
                        super::SnapSource::Layer => Color32::from_rgb(219, 115, 213),
                    };
                    painter.line_segment(line, Stroke::new(1.0_f32, color));
                }
                if let Some((start, end)) = self.crop_rect {
                    let rect = Rect::from_two_pos(map(start), map(end));
                    painter.rect_stroke(
                        rect,
                        0.0,
                        Stroke::new(1.5_f32, Color32::WHITE),
                        StrokeKind::Inside,
                    );
                    for f in [1.0 / 3.0, 2.0 / 3.0] {
                        painter.line_segment(
                            [
                                pos2(rect.left() + rect.width() * f, rect.top()),
                                pos2(rect.left() + rect.width() * f, rect.bottom()),
                            ],
                            Stroke::new(0.7_f32, Color32::from_white_alpha(140)),
                        );
                        painter.line_segment(
                            [
                                pos2(rect.left(), rect.top() + rect.height() * f),
                                pos2(rect.right(), rect.top() + rect.height() * f),
                            ],
                            Stroke::new(0.7_f32, Color32::from_white_alpha(140)),
                        );
                    }
                }
                if let Some(gesture) = &self.gesture {
                    let rect = Rect::from_two_pos(map(gesture.start), map(gesture.last));
                    if matches!(self.tool, Tool::Marquee | Tool::Shape)
                        && !matches!(gesture.kind, TransformDrag::Selection)
                    {
                        if self.tool == Tool::Shape
                            && self.shape_kind == mectov::paint::ShapeKind::Line
                        {
                            painter.add(egui::Shape::line(
                                vec![map(gesture.start), map(gesture.last)],
                                Stroke::new((self.line_width * zoom).max(1.0), Color32::WHITE),
                            ));
                            painter.circle_filled(map(gesture.start), 3.0, Color32::WHITE);
                            painter.circle_filled(map(gesture.last), 3.0, Color32::WHITE);
                        } else if (self.tool == Tool::Marquee && self.ellipse)
                            || (self.tool == Tool::Shape
                                && self.shape_kind == mectov::paint::ShapeKind::Ellipse)
                        {
                            painter.add(egui::epaint::EllipseShape::stroke(
                                rect.center(),
                                rect.size() * 0.5,
                                Stroke::new(1.0_f32, Color32::WHITE),
                            ));
                        } else {
                            painter.rect_stroke(
                                rect,
                                0.0,
                                Stroke::new(1.0_f32, Color32::WHITE),
                                StrokeKind::Inside,
                            );
                        }
                    }
                    if matches!(self.tool, Tool::Lasso | Tool::Heal) && gesture.points.len() > 1 {
                        painter.add(egui::Shape::line(
                            gesture.points.iter().copied().map(map).collect(),
                            Stroke::new(1.0_f32, Color32::WHITE),
                        ));
                    }
                    if self.tool == Tool::Gradient {
                        painter.line_segment(
                            [map(gesture.start), map(gesture.last)],
                            Stroke::new(1.5_f32, Color32::WHITE),
                        );
                        painter.circle_filled(map(gesture.start), 3.0, Color32::WHITE);
                        painter.circle_filled(map(gesture.last), 3.0, Color32::WHITE);
                    }
                }
                if !self.polygon.is_empty() {
                    let mut points: Vec<_> = self.polygon.iter().copied().map(map).collect();
                    if let Some(p) = response.hover_pos() {
                        points.push(p);
                    }
                    painter.add(egui::Shape::line(
                        points,
                        Stroke::new(1.0_f32, Color32::WHITE),
                    ));
                }
                if let Some(source) = self.clone_source {
                    let p = map(source);
                    painter.line_segment(
                        [p - vec2(5.0, 0.0), p + vec2(5.0, 0.0)],
                        Stroke::new(1.0_f32, Color32::WHITE),
                    );
                    painter.line_segment(
                        [p - vec2(0.0, 5.0), p + vec2(0.0, 5.0)],
                        Stroke::new(1.0_f32, Color32::WHITE),
                    );
                }
                let blocked = self.job.is_some()
                    || self.develop.is_some()
                    || self.dialog.is_some()
                    || self.error.is_some()
                    || self.close_app
                    || self.close_tab.is_some()
                    || self.rename.is_some();
                if blocked {
                    self.guide_drag = None;
                    return;
                }
                let global_pointer = ctx.input(|input| input.pointer.hover_pos());
                let primary_down =
                    ctx.input(|input| input.pointer.button_down(egui::PointerButton::Primary));
                if self.guide_drag.is_none() && primary_down {
                    let press = ctx
                        .input(|input| input.pointer.press_origin())
                        .or(global_pointer);
                    let dragged_guide = press.and_then(|press| {
                        if self.tool == Tool::Move && canvas_area.contains(press) {
                            let doc_press = Point::new(
                                (press.x - origin.x) / zoom,
                                (press.y - origin.y) / zoom,
                            );
                            let session = &self.sessions[self.current];
                            session.document.guides.iter().position(|guide| {
                                let distance = match guide.axis {
                                    GuideAxis::Vertical => (guide.position - doc_press.x).abs(),
                                    GuideAxis::Horizontal => (guide.position - doc_press.y).abs(),
                                };
                                distance * zoom <= GUIDE_GRAB_PX
                            })
                        } else {
                            None
                        }
                    });
                    let axis = press.and_then(|press| {
                        if horizontal_ruler.contains(press) {
                            Some(GuideAxis::Vertical)
                        } else if vertical_ruler.contains(press) {
                            Some(GuideAxis::Horizontal)
                        } else {
                            None
                        }
                    });
                    if let Some(index) = dragged_guide {
                        let guide = self.sessions[self.current].document.guides[index];
                        self.guide_drag = Some(GuideDrag {
                            axis: guide.axis,
                            position: guide.position,
                            existing: Some(index),
                        });
                    } else if let (Some(axis), Some(pointer)) = (axis, global_pointer) {
                        let value = match axis {
                            GuideAxis::Vertical => (pointer.x - origin.x) / zoom,
                            GuideAxis::Horizontal => (pointer.y - origin.y) / zoom,
                        };
                        let step = nice_step(10.0 / zoom);
                        self.guide_drag = Some(GuideDrag {
                            axis,
                            position: (value / step).round() * step,
                            existing: None,
                        });
                    }
                }
                if let Some(drag) = self.guide_drag {
                    let position = global_pointer.map(|pointer| {
                        let value = match drag.axis {
                            GuideAxis::Vertical => (pointer.x - origin.x) / zoom,
                            GuideAxis::Horizontal => (pointer.y - origin.y) / zoom,
                        };
                        let step = nice_step(10.0 / zoom);
                        ((value / step).round() * step).clamp(-1_000_000.0, 1_000_000.0)
                    });
                    if primary_down {
                        if let Some(position) = position {
                            self.guide_drag = Some(GuideDrag {
                                axis: drag.axis,
                                position,
                                existing: drag.existing,
                            });
                        }
                    } else {
                        let released_inside =
                            global_pointer.is_some_and(|p| canvas_area.contains(p));
                        let guide = Guide {
                            axis: drag.axis,
                            position: position.unwrap_or(drag.position),
                        };
                        pending_guide = Some(match (drag.existing, released_inside) {
                            (Some(index), true) => GuideEdit::Move(index, guide),
                            (Some(index), false) => GuideEdit::Delete(index),
                            (None, _) => GuideEdit::Create(guide),
                        });
                        self.guide_drag = None;
                    }
                }
                let pointer = response
                    .interact_pointer_pos()
                    .or_else(|| ctx.input(|i| i.pointer.hover_pos()));
                let doc_point =
                    pointer.map(|p| Point::new((p.x - origin.x) / zoom, (p.y - origin.y) / zoom));
                let modifiers = ctx.input(|i| i.modifiers);
                let panning = self.tool == Tool::Hand
                    || ctx.input(|i| i.key_down(egui::Key::Space))
                    || ctx.input(|i| i.pointer.button_down(egui::PointerButton::Middle));
                if response.hovered() {
                    let scroll = ctx.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta));
                    if scroll != Vec2::ZERO {
                        let session = &mut self.sessions[self.current];
                        let old = session.zoom;
                        let new = (old * (scroll.y * 0.003).exp()).clamp(0.01, 64.0);
                        if let Some(point) = doc_point {
                            session.pan -= (vec2(
                                point.x - session.document.width as f32 * 0.5,
                                point.y - session.document.height as f32 * 0.5,
                            )) * (new - old);
                        }
                        session.pan.x += scroll.x;
                        session.zoom = new;
                        session.fit = false;
                    }
                    let guide_axis = if self.tool == Tool::Move {
                        doc_point.and_then(|point| {
                            self.sessions[self.current]
                                .document
                                .guides
                                .iter()
                                .find(|guide| {
                                    let distance = match guide.axis {
                                        GuideAxis::Vertical => (guide.position - point.x).abs(),
                                        GuideAxis::Horizontal => (guide.position - point.y).abs(),
                                    };
                                    distance * zoom <= GUIDE_GRAB_PX
                                })
                                .map(|guide| guide.axis)
                        })
                    } else {
                        None
                    };
                    let cursor = if panning {
                        egui::CursorIcon::Grab
                    } else if let Some(axis) = guide_axis {
                        match axis {
                            GuideAxis::Vertical => egui::CursorIcon::ResizeHorizontal,
                            GuideAxis::Horizontal => egui::CursorIcon::ResizeVertical,
                        }
                    } else if hover_handle.is_some() {
                        egui::CursorIcon::ResizeNwSe
                    } else if self.tool == Tool::Move {
                        egui::CursorIcon::Move
                    } else if self.tool == Tool::Text {
                        egui::CursorIcon::Text
                    } else {
                        egui::CursorIcon::Crosshair
                    };
                    ctx.set_cursor_icon(cursor);
                    if self.tool.is_brush()
                        && !panning
                        && let Some(p) = pointer
                    {
                        painter.circle_stroke(
                            p,
                            self.brush.diameter * zoom * 0.5,
                            Stroke::new(2.5_f32, Color32::from_black_alpha(130)),
                        );
                        painter.circle_stroke(
                            p,
                            self.brush.diameter * zoom * 0.5,
                            Stroke::new(1.0_f32, Color32::WHITE),
                        );
                    }
                }
                let manipulating_guide = self.guide_drag.is_some() || pending_guide.is_some();
                let started = (response.drag_started()
                    || response.drag_started_by(egui::PointerButton::Middle))
                    && !manipulating_guide;
                if started {
                    if let (Some(screen), Some(point)) = (pointer, doc_point) {
                        let press = ctx.input(|i| i.pointer.press_origin()).unwrap_or(screen);
                        let start =
                            Point::new((press.x - origin.x) / zoom, (press.y - origin.y) / zoom);
                        self.begin_gesture(start, press, panning, hover_handle, modifiers);
                        self.update_gesture(point, screen, modifiers);
                    }
                } else if self.gesture.is_some()
                    && ctx.input(|i| i.pointer.any_down())
                    && let (Some(screen), Some(point)) = (pointer, doc_point)
                {
                    self.update_gesture(point, screen, modifiers);
                }
                if self.gesture.is_some() && !ctx.input(|i| i.pointer.any_down()) {
                    self.end_gesture(modifiers);
                }
                if response.clicked()
                    && !panning
                    && !manipulating_guide
                    && hover_handle.is_none()
                    && let Some(point) = doc_point
                {
                    self.canvas_click(point, modifiers);
                }
                if response.double_clicked() && self.tool == Tool::Lasso && self.polygonal {
                    self.finish_polygon();
                }
                if response.double_clicked()
                    && self.tool == Tool::Move
                    && !panning
                    && let Some(point) = doc_point
                    && let Some(id) =
                        render::hit_test_bounds(&self.sessions[self.current].document, point)
                    && self.sessions[self.current]
                        .document
                        .layers
                        .iter()
                        .any(|l| l.id == id && l.raw.is_some())
                {
                    self.start_develop_layer(id);
                }
                if !ctx.input(|i| i.raw.hovered_files.is_empty()) {
                    painter.rect_stroke(
                        viewport.shrink(5.0),
                        8.0,
                        Stroke::new(2.0_f32, theme::ACCENT),
                        StrokeKind::Inside,
                    );
                }
            });
        match pending_guide {
            Some(GuideEdit::Create(guide)) => {
                self.edit("New Guide", |doc| {
                    anyhow::ensure!(doc.guides.len() < MAX_GUIDES, "Too many guides");
                    doc.guides.push(guide);
                    Ok(())
                });
            }
            Some(GuideEdit::Move(index, guide)) => {
                self.edit("Move Guide", |doc| {
                    anyhow::ensure!(index < doc.guides.len(), "Guide no longer exists");
                    *doc.guides.get_mut(index).expect("checked above") = guide;
                    Ok(())
                });
            }
            Some(GuideEdit::Delete(index)) => {
                self.edit("Delete Guide", |doc| {
                    anyhow::ensure!(index < doc.guides.len(), "Guide no longer exists");
                    doc.guides.remove(index);
                    Ok(())
                });
            }
            None => {}
        }
    }

    fn welcome(&mut self, ui: &mut egui::Ui, viewport: Rect) {
        let width = 500.0_f32.min(viewport.width() - 40.0);
        let rect = Rect::from_center_size(viewport.center(), vec2(width, 260.0));
        let mut create = false;
        let mut open = false;
        let mut import = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.heading("New canvas");
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("A blank space for your next composition.")
                    .size(14.0)
                    .color(theme::MUTED),
            );
            ui.add_space(24.0);
            egui::Grid::new("welcome_dimensions")
                .num_columns(3)
                .min_col_width(0.0)
                .min_row_height(0.0)
                .show(ui, |ui| {
                    ui.label("Width");
                    ui.label("");
                    ui.label("Height");
                    ui.end_row();

                    ui.add_sized(
                        vec2(180.0, 36.0),
                        widgets::Number::new(&mut self.dimensions[0])
                            .range(1..=30_000)
                            .suffix(" px"),
                    );
                    ui.label("×");
                    ui.add_sized(
                        vec2(180.0, 36.0),
                        widgets::Number::new(&mut self.dimensions[1])
                            .range(1..=30_000)
                            .suffix(" px"),
                    );
                    ui.end_row();
                });
            ui.add_space(15.0);
            ui.label(egui::RichText::new("Transparent canvas · sRGB").color(theme::MUTED));
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                open = widgets::button(ui, "Open project").clicked();
                import = widgets::button(ui, "Import image").clicked();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    create = ui
                        .add(widgets::Button::new("Create canvas").primary())
                        .clicked();
                });
            });
        });
        if create {
            self.new_document();
        }
        if open {
            self.open_dialog(false);
        }
        if import {
            self.open_dialog(false);
        }
    }

    fn canvas_click(&mut self, point: Point, modifiers: egui::Modifiers) {
        match self.tool {
            Tool::Text => self.text_click(point),
            Tool::Move => {
                if self.auto_select || modifiers.ctrl {
                    self.select_canvas_layer(point, modifiers.shift, false);
                }
            }
            Tool::Wand => {
                let tolerance = self.tolerance;
                let contiguous = self.contiguous;
                let mode = self.selection_mode(modifiers);
                self.edit_selection("Magic Wand", |doc| {
                    let pixels = render::render(doc);
                    selection::combine(
                        doc,
                        selection::wand(&pixels, point, tolerance, contiguous),
                        mode,
                    );
                });
            }
            Tool::Dropper => {
                if let Some(session) = self.session() {
                    let pixel = render::pixel_at(&session.document, point);
                    self.brush.color = pixel.map(|v| (v * 255.0).round() as u8);
                }
            }
            Tool::Zoom => {
                if let Some(s) = self.session_mut() {
                    s.zoom = (s.zoom * if modifiers.alt { 0.8 } else { 1.25 }).clamp(0.01, 64.0);
                }
            }
            Tool::Lasso if self.polygonal => {
                if self.polygon.len() > 2
                    && self.polygon[0].distance(point) * self.session().unwrap().zoom < 8.0
                {
                    self.finish_polygon();
                } else {
                    self.polygon.push(point);
                }
            }
            Tool::Clone if modifiers.alt => {
                self.clone_source = Some(point);
                self.clone_offset = None;
            }
            tool if tool.is_brush() => {
                let from = if modifiers.shift {
                    self.last_brush.unwrap_or(point)
                } else {
                    point
                };
                self.begin_gesture(from, Pos2::ZERO, false, None, modifiers);
                self.update_gesture(point, Pos2::ZERO, modifiers);
                self.end_gesture(modifiers);
            }
            _ => {}
        }
    }

    fn select_canvas_layer(&mut self, point: Point, extend: bool, dragging: bool) -> bool {
        let ignore_transparent_pixels = self.ignore_transparent_pixels;
        let Some(session) = self.session_mut() else {
            return false;
        };
        let document = &mut session.document;
        let inside = point.x >= 0.0
            && point.y >= 0.0
            && point.x < document.width as f32
            && point.y < document.height as f32;
        let hit = inside
            .then(|| {
                if ignore_transparent_pixels {
                    render::hit_test(document, point)
                } else {
                    render::hit_test_bounds(document, point)
                }
            })
            .flatten();
        if let Some(id) = hit {
            // Moving an already selected layer keeps the other selected layers and mask target.
            if !dragging || !document.transform_targets().contains(&id) {
                document.select(id, extend);
                self.mask_target = false;
            }
        } else if !extend {
            document.selected.clear();
            document.active = None;
            self.mask_target = false;
        }
        hit.is_some()
    }

    fn selection_mode(&self, modifiers: egui::Modifiers) -> SelectionMode {
        if modifiers.shift {
            SelectionMode::Add
        } else if modifiers.alt {
            SelectionMode::Subtract
        } else {
            self.selection_mode
        }
    }

    pub(super) fn finish_polygon(&mut self) {
        if self.polygon.len() < 3 {
            return;
        }
        let points = std::mem::take(&mut self.polygon);
        let mode = self.selection_mode;
        self.edit_selection("Polygonal Lasso", |doc| {
            selection::combine(
                doc,
                selection::polygon(doc.width, doc.height, &points),
                mode,
            );
        });
    }

    fn begin_gesture(
        &mut self,
        mut point: Point,
        screen: Pos2,
        panning: bool,
        mut handle: Option<TransformDrag>,
        modifiers: egui::Modifiers,
    ) {
        if self.gesture.is_some() || self.sessions.is_empty() {
            return;
        }
        if panning {
            // View navigation must not start an edit or run tool-specific setup.
            let session = &self.sessions[self.current];
            self.gesture = Some(Gesture {
                start: point,
                last: point,
                screen_start: screen,
                pan_start: session.pan,
                points: Vec::new(),
                original: session.document.clone(),
                kind: TransformDrag::Move,
                panning: true,
                clone_offset: Point::default(),
                source: None,
                reference: None,
            });
            return;
        }
        if self.tool == Tool::Text {
            return;
        }
        if self.tool == Tool::Clone && modifiers.alt {
            self.clone_source = Some(point);
            self.clone_offset = None;
            return;
        }
        if self.tool == Tool::Clone && self.clone_source.is_none() {
            self.status = "Alt-click on the canvas to set a clone source".into();
            return;
        }
        if self.tool == Tool::Crop
            && let Some(selection) = self
                .session()
                .and_then(|session| session.document.selection.as_ref())
            && let Some((left, top, _, _)) = selection::bounds(selection)
        {
            point = Point::new(left as f32, top as f32);
        }
        if self.tool == Tool::Move && self.show_controls {
            // The press location determines the handle, even if the pointer has moved since.
            handle = None;
            let session = &self.sessions[self.current];
            if let Some(t) = operations::transform_box(&session.document, self.mask_target) {
                if let Some(index) = HANDLES
                    .iter()
                    .position(|unit| t.point(*unit).distance(point) * session.zoom < 9.0)
                {
                    handle = Some(if modifiers.ctrl && index % 2 == 0 {
                        TransformDrag::Distort(index / 2)
                    } else {
                        TransformDrag::Scale(index)
                    });
                } else {
                    let top = t.point(Point::new(0.5, 0.0));
                    let center = t.center();
                    let distance = top.distance(center).max(0.01);
                    let rotate = Point::new(
                        top.x + (top.x - center.x) / distance * 23.0 / session.zoom,
                        top.y + (top.y - center.y) / distance * 23.0 / session.zoom,
                    );
                    if rotate.distance(point) * session.zoom < 9.0 {
                        handle = Some(TransformDrag::Rotate);
                    }
                }
            }
        }
        let mut kind = handle.unwrap_or(TransformDrag::Move);
        if self.tool == Tool::Move
            && (self.auto_select || modifiers.ctrl)
            && handle.is_none()
            && !self.select_canvas_layer(point, modifiers.shift, true)
        {
            return;
        }
        let session = &mut self.sessions[self.current];
        if self.tool == Tool::Move && session.document.active.is_none() {
            return;
        }
        session.history.begin(self.tool.label(), &session.document);
        if self.tool == Tool::Move && modifiers.alt {
            operations::duplicate(&mut session.document);
        }
        if self.tool.is_selection()
            && !modifiers.shift
            && (!modifiers.alt || modifiers.ctrl)
            && session
                .document
                .selection
                .as_ref()
                .is_some_and(|m| selection::coverage(Some(m), point) > 0.0)
        {
            if modifiers.ctrl {
                if let Some((pixels, origin)) = operations::copy_pixels(&session.document, false) {
                    if !modifiers.alt
                        && let Err(error) = paint::fill(&mut session.document, [0; 4], true, false)
                    {
                        session.history.cancel(&mut session.document);
                        self.error = Some(error.to_string());
                        return;
                    }
                    let mut layer = mectov::document::Layer::image("Selection", pixels);
                    layer.transform.x = origin.x;
                    layer.transform.y = origin.y;
                    session.document.insert(layer);
                    session.document.selection = None;
                    kind = TransformDrag::Pixels;
                }
            } else {
                kind = TransformDrag::Selection;
            }
        }
        let source = if matches!(self.tool, Tool::Clone | Tool::Blur) {
            if self.tool == Tool::Clone && self.clone_all {
                Some(Arc::new(render::render(&session.document)))
            } else {
                let mut isolated = session.document.clone();
                let id = isolated.active;
                for l in &mut isolated.layers {
                    l.visible = Some(l.id) == id || l.group;
                }
                Some(Arc::new(render::render(&isolated)))
            }
        } else {
            None
        };
        let offset = if self.clone_aligned {
            self.clone_offset
        } else {
            None
        }
        .unwrap_or_else(|| {
            self.clone_source.map_or(Point::default(), |p| {
                Point::new(p.x - point.x, p.y - point.y)
            })
        });
        if self.tool == Tool::Clone {
            self.clone_offset = Some(offset);
        }
        self.gesture = Some(Gesture {
            start: point,
            last: point,
            screen_start: screen,
            pan_start: session.pan,
            points: if matches!(self.tool, Tool::Brush | Tool::Erase) {
                Vec::new()
            } else {
                vec![point]
            },
            original: session.document.clone(),
            kind,
            panning: false,
            clone_offset: offset,
            source,
            reference: operations::transform_box(&session.document, self.mask_target),
        });
    }

    fn update_gesture(&mut self, mut point: Point, screen: Pos2, modifiers: egui::Modifiers) {
        let Some(mut gesture) = self.gesture.take() else {
            return;
        };
        if gesture.panning {
            self.sessions[self.current].pan = gesture.pan_start + (screen - gesture.screen_start);
            self.gesture = Some(gesture);
            return;
        }
        if modifiers.shift && matches!(self.tool, Tool::Marquee | Tool::Crop) {
            let dx = point.x - gesture.start.x;
            let dy = point.y - gesture.start.y;
            let size = dx.abs().max(dy.abs());
            point = Point::new(
                gesture.start.x + size * dx.signum(),
                gesture.start.y + size * dy.signum(),
            );
        }
        if modifiers.shift && self.tool == Tool::Shape {
            point = constrain_shape_point(gesture.start, point, self.shape_kind);
        }
        if self.tool == Tool::Crop
            && let Some(ratio) = self.crop_ratio
        {
            point = constrain_crop_point(gesture.start, point, ratio);
        }
        let session = &mut self.sessions[self.current];
        let result = if matches!(gesture.kind, TransformDrag::Selection) && self.tool.is_selection()
        {
            if let Some(mask) = &gesture.original.selection {
                session.document.selection = Some(Arc::new(selection::translate(
                    mask,
                    (point.x - gesture.start.x).round() as i32,
                    (point.y - gesture.start.y).round() as i32,
                )));
            }
            Ok(())
        } else {
            match self.tool {
                Tool::Heal => {
                    gesture.points.push(point);
                    Ok(())
                }
                tool if tool.is_brush() => {
                    let mode = match tool {
                        Tool::Erase => PaintMode::Erase,
                        Tool::Clone => PaintMode::Clone,
                        Tool::Heal => PaintMode::Heal,
                        Tool::Blur => self.blur_mode,
                        _ => PaintMode::Paint,
                    };
                    let offset = if mode == PaintMode::Smudge {
                        Point::new(gesture.last.x - point.x, gesture.last.y - point.y)
                    } else {
                        gesture.clone_offset
                    };
                    let smoothing =
                        matches!(tool, Tool::Brush | Tool::Erase) && self.brush.smoothing > 0.0;
                    if smoothing {
                        (|| -> anyhow::Result<()> {
                            if gesture.points.is_empty() && gesture.last == gesture.start {
                                paint::stroke(
                                    &mut session.document,
                                    gesture.start,
                                    gesture.start,
                                    &self.brush,
                                    paint::StrokeOptions {
                                        mode,
                                        mask_target: self.mask_target,
                                        source: gesture.source.as_deref(),
                                        clone_offset: offset,
                                    },
                                )?;
                            }
                            record_smoothing_point(&mut gesture.points, point);
                            if let Some(painted) = paint::smooth_stroke_point(
                                gesture.last,
                                point,
                                self.brush.smoothing,
                                session.zoom,
                            ) {
                                paint::stroke(
                                    &mut session.document,
                                    gesture.last,
                                    painted,
                                    &self.brush,
                                    paint::StrokeOptions {
                                        mode,
                                        mask_target: self.mask_target,
                                        source: gesture.source.as_deref(),
                                        clone_offset: offset,
                                    },
                                )?;
                                gesture.last = painted;
                            }
                            Ok(())
                        })()
                    } else {
                        paint::stroke(
                            &mut session.document,
                            gesture.last,
                            point,
                            &self.brush,
                            paint::StrokeOptions {
                                mode,
                                mask_target: self.mask_target,
                                source: gesture.source.as_deref(),
                                clone_offset: offset,
                            },
                        )
                    }
                }
                _ if self.tool == Tool::Move || matches!(gesture.kind, TransformDrag::Pixels) => {
                    let mut dx = point.x - gesture.start.x;
                    let mut dy = point.y - gesture.start.y;
                    if modifiers.shift && matches!(gesture.kind, TransformDrag::Move) {
                        if dx.abs() > dy.abs() {
                            dy = 0.0;
                        } else {
                            dx = 0.0;
                        }
                    }
                    self.snap_indicators.clear();
                    if self.snap
                        && !modifiers.ctrl
                        && matches!(gesture.kind, TransformDrag::Move)
                        && let Some(t) = gesture.reference
                    {
                        let mut xs = Vec::new();
                        let mut ys = Vec::new();
                        if self.snap_targets.canvas {
                            xs.extend([
                                0.0,
                                session.document.width as f32 * 0.5,
                                session.document.width as f32,
                            ]);
                            ys.extend([
                                0.0,
                                session.document.height as f32 * 0.5,
                                session.document.height as f32,
                            ]);
                        }
                        if self.snap_targets.layers {
                            for l in &gesture.original.layers {
                                if !gesture.original.selected.contains(&l.id) && l.visible {
                                    xs.extend([
                                        l.transform.x,
                                        l.transform.center().x,
                                        l.transform.x + l.transform.width,
                                    ]);
                                    ys.extend([
                                        l.transform.y,
                                        l.transform.center().y,
                                        l.transform.y + l.transform.height,
                                    ]);
                                }
                            }
                        }
                        let guide_xs = gesture
                            .original
                            .guides
                            .iter()
                            .filter(|guide| guide.axis == GuideAxis::Vertical)
                            .map(|guide| guide.position)
                            .collect::<Vec<_>>();
                        let guide_ys = gesture
                            .original
                            .guides
                            .iter()
                            .filter(|guide| guide.axis == GuideAxis::Horizontal)
                            .map(|guide| guide.position)
                            .collect::<Vec<_>>();
                        let grid_step = self
                            .snap_targets
                            .grid
                            .then_some(gesture.original.grid)
                            .flatten()
                            .and_then(|grid| effective_grid_step(grid, session.zoom));
                        let snap = |values: [f32; 3],
                                    targets: &[f32],
                                    guides: &[f32],
                                    grid: Option<f32>|
                         -> Option<(f32, f32, SnapSource)> {
                            let threshold = 6.0 / session.zoom;
                            let mut best: Option<(f32, f32, SnapSource)> = None;
                            let consider = |value: f32,
                                                 target: f32,
                                                 source: SnapSource,
                                                 best: &mut Option<(f32, f32, SnapSource)>| {
                                let delta = target - value;
                                if delta.abs() < threshold
                                    && best.is_none_or(|current| delta.abs() < current.0.abs())
                                {
                                    *best = Some((delta, target, source));
                                }
                            };
                            for value in values {
                                for target in targets {
                                    consider(value, *target, SnapSource::Layer, &mut best);
                                }
                                if self.snap_targets.guides {
                                    for target in guides {
                                        consider(value, *target, SnapSource::Guide, &mut best);
                                    }
                                }
                                if let Some(step) = grid {
                                    consider(
                                        value,
                                        (value / step).round() * step,
                                        SnapSource::Grid,
                                        &mut best,
                                    );
                                }
                            }
                            best
                        };
                        if let Some((delta, x, source)) = snap(
                            [t.x + dx, t.center().x + dx, t.x + t.width + dx],
                            &xs,
                            &guide_xs,
                            grid_step,
                        ) {
                            dx += delta;
                            self.snap_indicators.push(SnapLine {
                                axis: GuideAxis::Vertical,
                                position: x,
                                source,
                            });
                        }
                        if let Some((delta, y, source)) = snap(
                            [t.y + dy, t.center().y + dy, t.y + t.height + dy],
                            &ys,
                            &guide_ys,
                            grid_step,
                        ) {
                            dy += delta;
                            self.snap_indicators.push(SnapLine {
                                axis: GuideAxis::Horizontal,
                                position: y,
                                source,
                            });
                        }
                    }
                    let targets = if self.mask_target {
                        gesture.original.active.into_iter().collect()
                    } else {
                        gesture.original.transform_targets()
                    };
                    if let Some(reference) = gesture.reference {
                        let moved = Point::new(gesture.start.x + dx, gesture.start.y + dy);
                        let transformed = drag_transform(
                            reference,
                            gesture.start,
                            moved,
                            gesture.kind,
                            self.lock_ratio,
                            modifiers.shift,
                        );
                        if transformed.valid() {
                            for layer in &mut session.document.layers {
                                if !targets.contains(&layer.id) || layer.locked {
                                    continue;
                                }
                                let Some(original) =
                                    gesture.original.layers.iter().find(|l| l.id == layer.id)
                                else {
                                    continue;
                                };
                                let old = if self.mask_target {
                                    original
                                        .mask
                                        .as_ref()
                                        .and_then(|m| m.placement)
                                        .unwrap_or(original.transform)
                                } else {
                                    original.transform
                                };
                                let transform = if targets.len() == 1 {
                                    transformed
                                } else {
                                    old.following(reference, transformed)
                                };
                                if self.mask_target {
                                    if let Some(mask) = &mut layer.mask {
                                        mask.placement = Some(transform);
                                        mask.linked = false;
                                    }
                                } else {
                                    layer.transform = original.transform;
                                    layer.mask = original.mask.clone();
                                    layer.set_transform(transform);
                                }
                            }
                        }
                    }
                    Ok(())
                }
                Tool::Lasso => {
                    if !self.polygonal {
                        gesture.points.push(point);
                    }
                    Ok(())
                }
                Tool::Crop => {
                    self.crop_rect = Some((gesture.start, point));
                    Ok(())
                }
                _ => Ok(()),
            }
        };
        if let Err(error) = result {
            session.history.cancel(&mut session.document);
            self.error = Some(error.to_string());
            session.invalidate();
            return;
        }
        gesture.last = point;
        if gesture.changes_composition(self.tool)
            && !matches!(self.tool, Tool::Gradient | Tool::Shape)
        {
            session.invalidate();
        }
        self.gesture = Some(gesture);
    }

    fn end_gesture(&mut self, modifiers: egui::Modifiers) {
        let Some(gesture) = self.gesture.take() else {
            return;
        };
        if gesture.panning {
            return;
        }
        if self.tool == Tool::Heal {
            let points = gesture.points;
            let brush = self.brush.clone();
            self.start_job("Spot Healing", move |document, cancel| {
                mectov::retouch::heal_path(document, &points, &brush, cancel)
            });
            return;
        }
        let mode = self.selection_mode(modifiers);
        let session = &mut self.sessions[self.current];
        let start = gesture.start;
        let end = if matches!(self.tool, Tool::Brush | Tool::Erase) && self.brush.smoothing > 0.0 {
            let pointer = gesture.points.last().copied().unwrap_or(gesture.last);
            if pointer != gesture.last {
                let mode = if self.tool == Tool::Erase {
                    PaintMode::Erase
                } else {
                    PaintMode::Paint
                };
                if let Err(error) = paint::stroke(
                    &mut session.document,
                    gesture.last,
                    pointer,
                    &self.brush,
                    paint::StrokeOptions {
                        mode,
                        mask_target: self.mask_target,
                        source: None,
                        clone_offset: Point::default(),
                    },
                ) {
                    session.history.cancel(&mut session.document);
                    self.error = Some(error.to_string());
                    session.invalidate();
                    self.snap_indicators.clear();
                    return;
                }
            }
            pointer
        } else {
            gesture.last
        };
        let changes_composition = gesture.changes_composition(self.tool);
        let result = if matches!(
            gesture.kind,
            TransformDrag::Selection | TransformDrag::Pixels
        ) && self.tool.is_selection()
        {
            Ok(())
        } else {
            match self.tool {
                Tool::Marquee => {
                    selection::combine(
                        &mut session.document,
                        selection::rectangle(
                            gesture.original.width,
                            gesture.original.height,
                            start,
                            end,
                            self.ellipse,
                        ),
                        mode,
                    );
                    Ok(())
                }
                Tool::Lasso if !self.polygonal => {
                    selection::combine(
                        &mut session.document,
                        selection::polygon(
                            gesture.original.width,
                            gesture.original.height,
                            &gesture.points,
                        ),
                        mode,
                    );
                    Ok(())
                }
                Tool::Gradient => paint::gradient(
                    &mut session.document,
                    start,
                    end,
                    paint::GradientOptions {
                        foreground: self.brush.color,
                        background: self.background,
                        radial: self.radial,
                        opacity: self.brush.opacity,
                        mask_target: self.mask_target,
                    },
                ),
                Tool::Shape => {
                    let start = if modifiers.alt {
                        Point::new(start.x - (end.x - start.x), start.y - (end.y - start.y))
                    } else {
                        start
                    };
                    paint::shape(
                        start,
                        end,
                        self.shape_kind,
                        self.brush.color,
                        self.corner_radius,
                        self.line_width,
                    )
                    .map(|layer| session.document.insert(layer))
                }
                Tool::Crop => {
                    session.history.cancel(&mut session.document);
                    return;
                }
                _ => Ok(()),
            }
        };
        if !changes_composition && result.is_ok() {
            session.history.commit();
            return;
        }
        match result {
            Ok(()) => match paint::refresh_shapes(&mut session.document) {
                Ok(()) => session.history.commit(),
                Err(error) => {
                    session.history.cancel(&mut session.document);
                    self.error = Some(error.to_string());
                }
            },
            Err(error) => {
                session.history.cancel(&mut session.document);
                self.error = Some(error.to_string());
            }
        }
        session.invalidate();
        self.snap_indicators.clear();
        if self.tool.is_brush() {
            self.last_brush = Some(end);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_ratio_constrains_both_drag_directions() {
        let start = Point::new(10.0, 20.0);
        let horizontal = constrain_crop_point(start, Point::new(30.0, 25.0), 16.0 / 9.0);
        assert!((horizontal.x - 30.0).abs() < 0.0001);
        assert!((horizontal.y - 31.25).abs() < 0.0001);
        let vertical = constrain_crop_point(start, Point::new(15.0, 50.0), 3.0 / 4.0);
        assert!((vertical.x - 32.5).abs() < 0.0001);
        assert!((vertical.y - 50.0).abs() < 0.0001);
    }

    #[test]
    fn smoothing_history_is_bounded_and_ignores_duplicate_samples() {
        let mut points = Vec::new();
        for x in 0..(MAX_SMOOTHING_HISTORY + 10) {
            record_smoothing_point(&mut points, Point::new(x as f32, 0.0));
        }
        record_smoothing_point(
            &mut points,
            Point::new((MAX_SMOOTHING_HISTORY + 9) as f32, 0.0),
        );
        assert_eq!(points.len(), MAX_SMOOTHING_HISTORY);
        assert_eq!(points.first(), Some(&Point::new(10.0, 0.0)));
    }
}
