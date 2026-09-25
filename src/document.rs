use std::{collections::HashSet, sync::Arc};

use anyhow::{Result, ensure};
use image::{GrayImage, RgbaImage};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::blend::BlendMode;

pub const MAX_SIDE: u32 = 30_000;
pub const MAX_PIXELS: u64 = 100_000_000;
pub const MAX_LAYERS: usize = 10_000;
pub const MAX_GUIDES: usize = 1_024;
pub const MAX_SHAPE_SIZE: f32 = 5_000.0;
/// Longest stroke width, shadow blur and glow size Compositor accepts, in document pixels.
pub const MAX_EFFECT_SIZE: f32 = 500.0;
/// Longest shadow distance Compositor accepts, in document pixels.
pub const MAX_EFFECT_DISTANCE: f32 = 5_000.0;
pub const DEFAULT_GRID_SPACING: f32 = 32.0;
pub const DEFAULT_GRID_SUBDIVISIONS: u32 = 1;
pub const MIN_GRID_SPACING: f32 = 1.0;
pub const MAX_GRID_SPACING: f32 = 5_000.0;
pub const MIN_GRID_SUBDIVISIONS: u32 = 1;
pub const MAX_GRID_SUBDIVISIONS: u32 = 100;

pub fn validate_size(width: u32, height: u32) -> Result<()> {
    ensure!(
        (1..=MAX_SIDE).contains(&width) && (1..=MAX_SIDE).contains(&height),
        "Dimensions must be between 1 and {MAX_SIDE} pixels"
    );
    ensure!(
        u64::from(width) * u64::from(height) <= MAX_PIXELS,
        "Images are limited to 100 megapixels"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn distance(self, other: Self) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub rotation: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    #[serde(default)]
    pub warp: Option<[Point; 4]>,
}

impl Transform {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: width as f32,
            height: height as f32,
            rotation: 0.0,
            flip_x: false,
            flip_y: false,
            warp: None,
        }
    }

    pub fn center(self) -> Point {
        Point::new(self.x + self.width * 0.5, self.y + self.height * 0.5)
    }

    /// Map normalized source coordinates to document coordinates.
    pub fn point(self, unit: Point) -> Point {
        let unit = self
            .warp
            .and_then(crate::geometry::Homography::from_quad)
            .map_or(unit, |h| h.map(unit));
        let x = (if self.flip_x { 1.0 - unit.x } else { unit.x } - 0.5) * self.width;
        let y = (if self.flip_y { 1.0 - unit.y } else { unit.y } - 0.5) * self.height;
        let (sin, cos) = self.rotation.to_radians().sin_cos();
        let center = self.center();
        Point::new(center.x + cos * x - sin * y, center.y + sin * x + cos * y)
    }

    pub fn inverse(self, point: Point) -> Point {
        let center = self.center();
        let x = point.x - center.x;
        let y = point.y - center.y;
        let (sin, cos) = self.rotation.to_radians().sin_cos();
        let mut u = (cos * x + sin * y) / self.width + 0.5;
        let mut v = (-sin * x + cos * y) / self.height + 0.5;
        if self.flip_x {
            u = 1.0 - u;
        }
        if self.flip_y {
            v = 1.0 - v;
        }
        let unit = Point::new(u, v);
        self.warp
            .and_then(crate::geometry::Homography::from_quad)
            .and_then(|h| h.inverse())
            .map_or(unit, |h| h.map(unit))
    }

    pub fn corners(self) -> [Point; 4] {
        [
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0),
            Point::new(1.0, 1.0),
            Point::new(0.0, 1.0),
        ]
        .map(|point| self.point(point))
    }

    pub fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height, self.rotation]
            .iter()
            .all(|x| x.is_finite())
            && (1.0..=300_000.0).contains(&self.width)
            && (1.0..=300_000.0).contains(&self.height)
            && self.x.abs() <= 1_000_000.0
            && self.y.abs() <= 1_000_000.0
            && self
                .warp
                .is_none_or(|quad| crate::geometry::Homography::from_quad(quad).is_some())
    }

    /// Enlarge the source grid without moving any of its existing pixels.
    pub fn expanded(self, left: f32, top: f32, right: f32, bottom: f32) -> Self {
        let corners = [
            Point::new(left, top),
            Point::new(right, top),
            Point::new(right, bottom),
            Point::new(left, bottom),
        ]
        .map(|p| self.point(p));
        let center = self.point(Point::new((left + right) * 0.5, (top + bottom) * 0.5));
        let mut result = self;
        result.width *= right - left;
        result.height *= bottom - top;
        result.x = center.x - result.width * 0.5;
        result.y = center.y - result.height * 0.5;
        result.warp = None;
        result.warp = Some(corners.map(|p| result.inverse(p)));
        result
    }

    pub fn following(self, old: Self, new: Self) -> Self {
        if old.width == new.width
            && old.height == new.height
            && old.rotation == new.rotation
            && old.flip_x == new.flip_x
            && old.flip_y == new.flip_y
            && old.warp == new.warp
        {
            return Self {
                x: self.x + new.x - old.x,
                y: self.y + new.y - old.y,
                ..self
            };
        }
        let map = |point| new.point(old.inverse(point));
        let center = map(self.center());
        let mut result = self;
        result.width = (self.width * new.width / old.width).max(1.0);
        result.height = (self.height * new.height / old.height).max(1.0);
        result.x = center.x - result.width * 0.5;
        result.y = center.y - result.height * 0.5;
        result.rotation += new.rotation - old.rotation;
        result.warp = None;
        let quad = self.corners().map(|p| result.inverse(map(p)));
        if crate::geometry::Homography::from_quad(quad).is_some() {
            result.warp = Some(quad);
        }
        result
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mask {
    #[serde(skip)]
    pub pixels: Arc<GrayImage>,
    pub enabled: bool,
    pub linked: bool,
    pub placement: Option<Transform>,
}

impl Mask {
    pub fn white() -> Self {
        Self {
            pixels: Arc::new(GrayImage::from_pixel(1, 1, image::Luma([255]))),
            enabled: true,
            linked: true,
            placement: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Adjustment {
    HueRanges {
        settings: Box<crate::color::HueSettings>,
    },
    LevelsChannels {
        ranges: [[f32; 5]; 4],
    },
    CurvesChannels {
        channels: [Vec<Point>; 4],
    },
    HueSaturation {
        hue: f32,
        saturation: f32,
        lightness: f32,
        colorize: bool,
    },
    Levels {
        black: f32,
        gamma: f32,
        white: f32,
        output_black: f32,
        output_white: f32,
    },
    Curves {
        points: Vec<Point>,
    },
    Exposure {
        exposure: f32,
        offset: f32,
        gamma: f32,
    },
    GradientMap {
        shadows: [u8; 4],
        highlights: [u8; 4],
    },
    FilmGrain {
        amount: f32,
        size: f32,
        roughness: f32,
        seed: u32,
    },
    Grain {
        amount: f32,
        monochrome: bool,
        seed: u32,
    },
    /// Digital noise over everything the adjustment covers. Compositor's Add Noise: `gaussian` swaps
    /// the even distribution for a bell curve, and a monochrome seed keeps one value per pixel.
    AddNoise {
        amount: f32,
        gaussian: bool,
        monochromatic: bool,
        seed: u32,
    },
    /// A blur of everything under the layer, not of the layer itself. Compositor draws these with Core
    /// Image at the scale the canvas is being drawn, so the radius is in document pixels.
    GaussianBlur {
        radius: f32,
    },
    MotionBlur {
        angle: f32,
        distance: f32,
    },
    Invert,
    /// Photoshop's Black & White: each family of colors has its own weight, so reds and greens stay
    /// apart instead of flattening into one gray. The weights are percentages, −200…300.
    BlackWhite {
        reds: f32,
        yellows: f32,
        greens: f32,
        cyans: f32,
        blues: f32,
        magentas: f32,
        tint: bool,
        tint_hue: f32,
        tint_saturation: f32,
    },
    /// Color Balance: one shift per opposing pair, separately for shadows, midtones and highlights.
    /// Each pair is cyan–red, magenta–green and yellow–blue, −100…100.
    ColorBalance {
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
        preserve_luminosity: bool,
    },
}

impl Adjustment {
    pub fn name(&self) -> &'static str {
        match self {
            Self::HueSaturation { .. } => "Hue/Saturation",
            Self::HueRanges { .. } => "Hue/Saturation",
            Self::LevelsChannels { .. } => "Levels",
            Self::CurvesChannels { .. } => "Curves",
            Self::Levels { .. } => "Levels",
            Self::Curves { .. } => "Curves",
            Self::Exposure { .. } => "Exposure",
            Self::GradientMap { .. } => "Gradient Map",
            Self::Grain { .. } | Self::FilmGrain { .. } => "Grain",
            Self::AddNoise { .. } => "Add Noise",
            Self::GaussianBlur { .. } => "Gaussian Blur",
            Self::MotionBlur { .. } => "Motion Blur",
            Self::Invert => "Invert",
            Self::BlackWhite { .. } => "Black & White",
            Self::ColorBalance { .. } => "Color Balance",
        }
    }

    /// The blur and noise adjustments redraw everything beneath them rather than the pixel they land
    /// on, so the compositor has to blur the backdrop before the layers above are drawn.
    pub fn is_backdrop_filter(&self) -> bool {
        matches!(self, Self::GaussianBlur { .. } | Self::MotionBlur { .. })
    }

    /// The defaults Photoshop opens with, which Compositor uses too.
    pub fn photoshop_black_white() -> Self {
        Self::BlackWhite {
            reds: 40.0,
            yellows: 60.0,
            greens: 40.0,
            cyans: 60.0,
            blues: 20.0,
            magentas: 80.0,
            tint: false,
            tint_hue: 40.0,
            tint_saturation: 20.0,
        }
    }

    pub fn neutral_color_balance() -> Self {
        Self::ColorBalance {
            shadows: [0.0; 3],
            midtones: [0.0; 3],
            highlights: [0.0; 3],
            preserve_luminosity: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapeStyle {
    pub kind: crate::paint::ShapeKind,
    pub color: [u8; 4],
    pub corner_radius: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<Point>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<Point>,
}

/// A user-placed alignment line. Horizontal guides sit at a document Y; vertical ones at a document X.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuideAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Guide {
    pub axis: GuideAxis,
    /// Document pixels: Y for a horizontal guide, X for a vertical one.
    pub position: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GridSettings {
    #[serde(default = "default_grid_spacing")]
    pub spacing: f32,
    #[serde(default = "default_grid_subdivisions")]
    pub subdivisions: u32,
}

impl Default for GridSettings {
    fn default() -> Self {
        Self {
            spacing: DEFAULT_GRID_SPACING,
            subdivisions: DEFAULT_GRID_SUBDIVISIONS,
        }
    }
}

impl GridSettings {
    pub fn minor_spacing(self) -> f32 {
        self.spacing / self.subdivisions as f32
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.spacing.is_finite()
                && (MIN_GRID_SPACING..=MAX_GRID_SPACING).contains(&self.spacing),
            "Invalid grid spacing"
        );
        ensure!(
            (MIN_GRID_SUBDIVISIONS..=MAX_GRID_SUBDIVISIONS).contains(&self.subdivisions),
            "Invalid grid subdivisions"
        );
        Ok(())
    }
}

fn visible_by_default() -> bool {
    true
}
fn default_grid_spacing() -> f32 {
    DEFAULT_GRID_SPACING
}
fn default_grid_subdivisions() -> u32 {
    DEFAULT_GRID_SUBDIVISIONS
}
fn amount_in_range(value: f32, maximum: f32) -> bool {
    value.is_finite() && (0.0..=maximum).contains(&value)
}
fn color_in_range(color: [f32; 3]) -> bool {
    color
        .iter()
        .all(|c| c.is_finite() && (0.0..=1.0).contains(c))
}
fn opacity_in_range(opacity: f32) -> bool {
    opacity.is_finite() && (0.0..=1.0).contains(&opacity)
}

/// A line drawn around what the layer shows, outside its edge or inside it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeEffect {
    #[serde(default = "visible_by_default")]
    pub enabled: bool,
    pub size: f32,
    pub color: [f32; 3],
    pub opacity: f32,
    pub inside: bool,
}

impl Default for StrokeEffect {
    fn default() -> Self {
        Self {
            enabled: true,
            size: 4.0,
            color: [0.0; 3],
            opacity: 1.0,
            inside: false,
        }
    }
}

/// The layer's shape repeated behind it, offset and softened.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShadowEffect {
    #[serde(default = "visible_by_default")]
    pub enabled: bool,
    /// Where the light comes from, in degrees counterclockwise from the right, as Photoshop's dial is:
    /// 90 is from straight above, which drops the shadow straight down.
    pub angle: f32,
    pub distance: f32,
    pub blur: f32,
    pub color: [f32; 3],
    pub opacity: f32,
}

impl Default for ShadowEffect {
    fn default() -> Self {
        Self {
            enabled: true,
            angle: 90.0,
            distance: 20.0,
            blur: 20.0,
            color: [0.0; 3],
            opacity: 0.5,
        }
    }
}

/// A flat color over everything the layer shows.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorOverlayEffect {
    #[serde(default = "visible_by_default")]
    pub enabled: bool,
    pub color: [f32; 3],
    pub opacity: f32,
}

impl Default for ColorOverlayEffect {
    fn default() -> Self {
        Self {
            enabled: true,
            color: [0.0; 3],
            opacity: 1.0,
        }
    }
}

/// A shadow cast inside the layer's own edges, as though it were cut out of what is behind it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct InnerShadowEffect {
    #[serde(default = "visible_by_default")]
    pub enabled: bool,
    pub angle: f32,
    pub distance: f32,
    pub blur: f32,
    pub color: [f32; 3],
    pub opacity: f32,
}

impl Default for InnerShadowEffect {
    fn default() -> Self {
        Self {
            enabled: true,
            angle: 90.0,
            distance: 10.0,
            blur: 10.0,
            color: [0.0; 3],
            opacity: 0.5,
        }
    }
}

/// A soft glow drawn omnidirectionally around the outside of what the layer shows.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OuterGlowEffect {
    #[serde(default = "visible_by_default")]
    pub enabled: bool,
    pub size: f32,
    pub color: [f32; 3],
    pub opacity: f32,
}

impl Default for OuterGlowEffect {
    fn default() -> Self {
        Self {
            enabled: true,
            size: 20.0,
            color: [1.0; 3],
            opacity: 0.75,
        }
    }
}

/// A soft glow drawn inside the layer's own edges, so its interior brightens away from where it ends.
/// Compositor added this in 1.2.3; mectov keeps it with the layer and draws no effects yet.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct InnerGlowEffect {
    #[serde(default = "visible_by_default")]
    pub enabled: bool,
    pub size: f32,
    pub color: [f32; 3],
    pub opacity: f32,
}

impl Default for InnerGlowEffect {
    fn default() -> Self {
        Self {
            enabled: true,
            size: 10.0,
            color: [1.0; 3],
            opacity: 0.75,
        }
    }
}

/// What a layer draws around itself, kept with the layer so it follows every edit and can be changed or
/// removed at any time. Compositor projects carry these on their layers; mectov preserves them and keeps
/// disabled ones, which Compositor hides rather than deletes. Drawing them is not implemented yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LayerEffects {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<StrokeEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<ShadowEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_overlay: Option<ColorOverlayEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_shadow: Option<InnerShadowEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_glow: Option<OuterGlowEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_glow: Option<InnerGlowEffect>,
}

impl LayerEffects {
    pub fn is_empty(&self) -> bool {
        self.stroke.is_none()
            && self.shadow.is_none()
            && self.color_overlay.is_none()
            && self.inner_shadow.is_none()
            && self.outer_glow.is_none()
            && self.inner_glow.is_none()
    }

    pub fn renders(&self) -> bool {
        self.stroke
            .as_ref()
            .is_some_and(|effect| effect.enabled && effect.size > 0.0 && effect.opacity > 0.0)
            || self
                .shadow
                .as_ref()
                .is_some_and(|effect| effect.enabled && effect.opacity > 0.0)
            || self
                .color_overlay
                .as_ref()
                .is_some_and(|effect| effect.enabled && effect.opacity > 0.0)
            || self
                .inner_shadow
                .as_ref()
                .is_some_and(|effect| effect.enabled && effect.opacity > 0.0)
            || self
                .outer_glow
                .as_ref()
                .is_some_and(|effect| effect.enabled && effect.size > 0.0 && effect.opacity > 0.0)
            || self
                .inner_glow
                .as_ref()
                .is_some_and(|effect| effect.enabled && effect.size > 0.0 && effect.opacity > 0.0)
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(stroke) = &self.stroke {
            ensure!(
                amount_in_range(stroke.size, MAX_EFFECT_SIZE),
                "Invalid stroke size"
            );
            ensure!(
                color_in_range(stroke.color) && opacity_in_range(stroke.opacity),
                "Invalid stroke color"
            );
        }
        for shadow in [
            self.shadow
                .map(|s| (s.angle, s.distance, s.blur, s.color, s.opacity)),
            self.inner_shadow
                .map(|s| (s.angle, s.distance, s.blur, s.color, s.opacity)),
        ]
        .into_iter()
        .flatten()
        {
            let (angle, distance, blur, color, opacity) = shadow;
            ensure!(
                angle.is_finite()
                    && (-360.0..=360.0).contains(&angle)
                    && amount_in_range(distance, MAX_EFFECT_DISTANCE)
                    && amount_in_range(blur, MAX_EFFECT_SIZE),
                "Invalid shadow geometry"
            );
            ensure!(
                color_in_range(color) && opacity_in_range(opacity),
                "Invalid shadow color"
            );
        }
        if let Some(overlay) = &self.color_overlay {
            ensure!(
                color_in_range(overlay.color) && opacity_in_range(overlay.opacity),
                "Invalid color overlay"
            );
        }
        if let Some(glow) = &self.outer_glow {
            ensure!(
                amount_in_range(glow.size, MAX_EFFECT_SIZE),
                "Invalid outer glow size"
            );
            ensure!(
                color_in_range(glow.color) && opacity_in_range(glow.opacity),
                "Invalid outer glow color"
            );
        }
        if let Some(glow) = &self.inner_glow {
            ensure!(
                amount_in_range(glow.size, MAX_EFFECT_SIZE),
                "Invalid inner glow size"
            );
            ensure!(
                color_in_range(glow.color) && opacity_in_range(glow.opacity),
                "Invalid inner glow color"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layer {
    pub id: Uuid,
    pub name: String,
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    pub blend: BlendMode,
    pub transform: Transform,
    pub parent: Option<Uuid>,
    pub group: bool,
    pub clip_to: Option<Uuid>,
    pub mask: Option<Mask>,
    pub adjustment: Option<Adjustment>,
    #[serde(default)]
    pub shape: Option<ShapeStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<crate::text::TextStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<crate::raw::RawAsset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<LayerEffects>,
    #[serde(skip)]
    pub pixels: Option<Arc<RgbaImage>>,
}

impl Layer {
    pub fn blank(name: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            visible: true,
            locked: false,
            opacity: 1.0,
            blend: BlendMode::Normal,
            transform: Transform::new(width, height),
            parent: None,
            group: false,
            clip_to: None,
            mask: None,
            adjustment: None,
            shape: None,
            text: None,
            raw: None,
            effects: None,
            pixels: None,
        }
    }

    pub fn set_transform(&mut self, transform: Transform) {
        if let Some(mask) = &mut self.mask {
            if mask.linked {
                mask.placement = mask
                    .placement
                    .map(|placement| placement.following(self.transform, transform));
            } else if mask.placement.is_none() {
                mask.placement = Some(self.transform);
            }
        }
        self.transform = transform;
    }

    pub fn image(name: impl Into<String>, pixels: RgbaImage) -> Self {
        let mut layer = Self::blank(name, pixels.width(), pixels.height());
        layer.pixels = Some(Arc::new(pixels));
        layer
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub id: Uuid,
    pub width: u32,
    pub height: u32,
    pub resolution: f32,
    pub layers: Vec<Layer>,
    pub active: Option<Uuid>,
    /// Alignment guides placed in the document, in the order a Compositor project stored them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guides: Vec<Guide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<GridSettings>,
    #[serde(skip)]
    pub selected: HashSet<Uuid>,
    #[serde(skip)]
    pub selection: Option<Arc<GrayImage>>,
}

impl Document {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        validate_size(width, height)?;
        let layer = Layer::blank("Layer 1", width, height);
        Ok(Self {
            id: Uuid::new_v4(),
            width,
            height,
            resolution: 72.0,
            active: Some(layer.id),
            selected: HashSet::from([layer.id]),
            layers: vec![layer],
            guides: Vec::new(),
            grid: None,
            selection: None,
        })
    }

    pub fn active(&self) -> Option<&Layer> {
        self.layers
            .iter()
            .find(|layer| Some(layer.id) == self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Layer> {
        self.layers
            .iter_mut()
            .find(|layer| Some(layer.id) == self.active)
    }

    pub fn select(&mut self, id: Uuid, extend: bool) {
        if !extend {
            self.selected.clear();
        }
        if extend && self.selected.contains(&id) {
            self.selected.remove(&id);
        } else {
            self.selected.insert(id);
        }
        self.active = if self.selected.contains(&id) {
            Some(id)
        } else {
            self.layers
                .iter()
                .rev()
                .find(|layer| self.selected.contains(&layer.id))
                .map(|layer| layer.id)
        };
    }

    pub fn insert(&mut self, mut layer: Layer) {
        let index = self
            .layers
            .iter()
            .position(|layer| Some(layer.id) == self.active);
        if let Some(active) = self.active() {
            layer.parent = if active.group {
                Some(active.id)
            } else {
                active.parent
            };
        }
        self.select(layer.id, false);
        self.layers
            .insert(index.map_or(self.layers.len(), |i| i + 1), layer);
    }

    pub fn descendants(&self, id: Uuid) -> HashSet<Uuid> {
        let mut result = HashSet::from([id]);
        loop {
            let old = result.len();
            for layer in &self.layers {
                if layer.parent.is_some_and(|parent| result.contains(&parent)) {
                    result.insert(layer.id);
                }
            }
            if result.len() == old {
                break;
            }
        }
        result
    }

    pub fn transform_targets(&self) -> HashSet<Uuid> {
        let mut result = self.selected.clone();
        for id in &self.selected {
            result.extend(self.descendants(*id));
        }
        result
    }

    pub fn delete_selected(&mut self) {
        let deleted = self.transform_targets();
        self.layers.retain(|layer| !deleted.contains(&layer.id));
        for layer in &mut self.layers {
            if layer.clip_to.is_some_and(|id| deleted.contains(&id)) {
                layer.clip_to = None;
            }
        }
        self.active = self.layers.last().map(|layer| layer.id);
        self.selected = self.active.into_iter().collect();
    }

    pub fn validate(&self) -> Result<()> {
        validate_size(self.width, self.height)?;
        ensure!(
            self.resolution.is_finite() && (1.0..=9600.0).contains(&self.resolution),
            "Invalid resolution"
        );
        ensure!(self.layers.len() <= MAX_LAYERS, "Too many layers");
        ensure!(self.guides.len() <= MAX_GUIDES, "Too many guides");
        for guide in &self.guides {
            ensure!(guide.position.is_finite(), "Invalid guide position");
        }
        if let Some(grid) = &self.grid {
            grid.validate()?;
        }
        let ids: HashSet<_> = self.layers.iter().map(|layer| layer.id).collect();
        ensure!(
            ids.len() == self.layers.len(),
            "Duplicate layer identifiers"
        );
        ensure!(
            self.active.is_none_or(|id| ids.contains(&id)),
            "Missing active layer"
        );
        let mut pixels = 0_u64;
        let mut mask_pixels = 0_u64;
        let mut raw_bytes = 0_u64;
        for layer in &self.layers {
            if let Some(raw) = &layer.raw {
                raw.validate()?;
                raw_bytes += raw.bytes.len() as u64;
                ensure!(
                    layer.pixels.is_some() && layer.text.is_none() && layer.shape.is_none(),
                    "Invalid RAW layer"
                );
            }
            if let Some(text) = &layer.text {
                text.validate()?;
                ensure!(
                    layer.pixels.is_some() && layer.shape.is_none(),
                    "Invalid text layer"
                );
            }
            if let Some(shape) = &layer.shape {
                let point_in_range = |point: &Point| {
                    point.x.is_finite()
                        && point.y.is_finite()
                        && (0.0..=1.0).contains(&point.x)
                        && (0.0..=1.0).contains(&point.y)
                };
                let line_geometry = if shape.kind == crate::paint::ShapeKind::Line {
                    shape.line_width.is_none_or(|width| {
                        width.is_finite() && (0.0..=MAX_SHAPE_SIZE).contains(&width)
                    }) && shape.start.as_ref().is_none_or(point_in_range)
                        && shape.end.as_ref().is_none_or(point_in_range)
                } else {
                    shape.line_width.is_none() && shape.start.is_none() && shape.end.is_none()
                };
                ensure!(
                    shape.corner_radius.is_finite()
                        && shape.corner_radius >= 0.0
                        && line_geometry
                        && layer.pixels.is_some(),
                    "Invalid live shape"
                );
            }
            if let Some(adjustment) = &layer.adjustment {
                crate::effects::validate_adjustment(adjustment)?;
            }
            if let Some(effects) = &layer.effects {
                effects.validate()?;
            }
            ensure!(
                !layer.name.trim().is_empty() && layer.name.len() <= 16_384,
                "Invalid layer name"
            );
            ensure!(layer.transform.valid(), "Invalid layer transform");
            ensure!(
                layer.opacity.is_finite() && (0.0..=1.0).contains(&layer.opacity),
                "Invalid opacity"
            );
            ensure!(
                !(layer.group || layer.adjustment.is_some()) || layer.pixels.is_none(),
                "Group/adjustment cannot contain pixels"
            );
            if let Some(image) = &layer.pixels {
                validate_size(image.width(), image.height())?;
                pixels += u64::from(image.width()) * u64::from(image.height());
            }
            if let Some(mask) = &layer.mask {
                validate_size(mask.pixels.width(), mask.pixels.height())?;
                ensure!(
                    mask.placement.is_none_or(Transform::valid),
                    "Invalid mask transform"
                );
                mask_pixels += u64::from(mask.pixels.width()) * u64::from(mask.pixels.height());
            }
            let mut parent = layer.parent;
            let mut visited = HashSet::from([layer.id]);
            while let Some(id) = parent {
                ensure!(
                    visited.insert(id) && visited.len() <= 65,
                    "Cyclic or excessively nested groups"
                );
                let group = self.layers.iter().find(|l| l.id == id);
                ensure!(group.is_some_and(|l| l.group), "Missing parent group");
                parent = group.and_then(|l| l.parent);
            }
            let mut source = layer.clip_to;
            let mut visited = HashSet::from([layer.id]);
            while let Some(id) = source {
                ensure!(
                    !layer.group && visited.insert(id) && visited.len() <= 257,
                    "Invalid clipping mask graph"
                );
                let target = self.layers.iter().find(|l| l.id == id);
                ensure!(target.is_some_and(|l| !l.group), "Missing clipping source");
                source = target.and_then(|l| l.clip_to);
            }
        }
        ensure!(
            pixels <= MAX_PIXELS && mask_pixels <= MAX_PIXELS,
            "Project exceeds the 100 megapixel asset limit"
        );
        ensure!(
            raw_bytes <= crate::raw::MAX_RAW_BYTES,
            "Project exceeds 512 MiB of RAW assets"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn transform_round_trip_with_rotation_and_flips() {
        for flip_x in [false, true] {
            let t = Transform {
                x: -13.0,
                y: 25.0,
                width: 90.0,
                height: 45.0,
                rotation: 37.0,
                flip_x,
                flip_y: true,
                warp: None,
            };
            let p = Point::new(0.17, 0.89);
            assert!(t.inverse(t.point(p)).distance(p) < 0.00001);
        }
    }

    #[test]
    fn rejects_invalid_hierarchy_and_clipping_cycles() {
        let mut doc = Document::new(8, 8).unwrap();
        doc.layers[0].clip_to = Some(doc.layers[0].id);
        assert!(doc.validate().is_err());
        doc.layers[0].clip_to = None;
        doc.layers[0].parent = Some(Uuid::new_v4());
        assert!(doc.validate().is_err());
    }

    #[test]
    fn rejects_invalid_guides_and_out_of_range_effects() {
        let mut doc = Document::new(8, 8).unwrap();
        doc.guides = vec![Guide {
            axis: GuideAxis::Horizontal,
            position: f32::NAN,
        }];
        assert!(doc.validate().is_err());
        doc.guides.clear();
        doc.layers[0].effects = Some(LayerEffects {
            stroke: Some(StrokeEffect {
                size: MAX_EFFECT_SIZE + 1.0,
                ..StrokeEffect::default()
            }),
            ..LayerEffects::default()
        });
        assert!(doc.validate().is_err());
        doc.layers[0].effects = Some(LayerEffects {
            inner_shadow: Some(InnerShadowEffect {
                angle: 400.0,
                ..InnerShadowEffect::default()
            }),
            ..LayerEffects::default()
        });
        assert!(doc.validate().is_err());
        doc.layers[0].effects = Some(LayerEffects {
            color_overlay: Some(ColorOverlayEffect {
                opacity: 1.5,
                ..ColorOverlayEffect::default()
            }),
            ..LayerEffects::default()
        });
        assert!(doc.validate().is_err());
        doc.layers[0].effects = Some(LayerEffects {
            stroke: Some(StrokeEffect::default()),
            outer_glow: Some(OuterGlowEffect::default()),
            ..LayerEffects::default()
        });
        doc.validate().unwrap();
        assert!(LayerEffects::default().is_empty());
    }

    #[test]
    fn deleting_group_removes_descendants_and_stale_links() {
        let mut doc = Document::new(8, 8).unwrap();
        doc.layers[0].group = true;
        let group = doc.layers[0].id;
        doc.insert(Layer::blank("Child", 8, 8));
        let child = doc.active.unwrap();
        let mut outside = Layer::blank("Outside", 8, 8);
        outside.clip_to = Some(child);
        doc.layers.push(outside);
        doc.select(group, false);
        doc.delete_selected();
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].clip_to, None);
        doc.validate().unwrap();
    }

    #[test]
    fn accepts_valid_grids_and_rejects_out_of_range_ones() {
        let mut doc = Document::new(8, 8).unwrap();
        assert_eq!(doc.grid, None);
        doc.grid = Some(GridSettings {
            spacing: MIN_GRID_SPACING,
            subdivisions: MAX_GRID_SUBDIVISIONS,
        });
        doc.validate().unwrap();
        assert_eq!(doc.grid.unwrap().minor_spacing(), 0.01);
        doc.grid = Some(GridSettings {
            spacing: MAX_GRID_SPACING,
            subdivisions: MIN_GRID_SUBDIVISIONS,
        });
        doc.validate().unwrap();

        for spacing in [
            0.0,
            MIN_GRID_SPACING - 0.5,
            MAX_GRID_SPACING + 0.5,
            f32::NAN,
            f32::INFINITY,
        ] {
            doc.grid = Some(GridSettings {
                spacing,
                subdivisions: 1,
            });
            assert!(doc.validate().is_err(), "spacing {spacing}");
        }
        for subdivisions in [0, MAX_GRID_SUBDIVISIONS + 1] {
            doc.grid = Some(GridSettings {
                spacing: 16.0,
                subdivisions,
            });
            assert!(doc.validate().is_err(), "subdivisions {subdivisions}");
        }
        doc.grid = None;
        doc.validate().unwrap();
    }

    #[test]
    fn reads_documents_saved_before_the_grid_existed() {
        assert_eq!(GridSettings::default().spacing, DEFAULT_GRID_SPACING);
        assert_eq!(
            GridSettings::default().subdivisions,
            DEFAULT_GRID_SUBDIVISIONS
        );
        let value = serde_json::json!({
            "id": Uuid::new_v4(),
            "width": 2,
            "height": 2,
            "resolution": 72.0,
            "layers": [],
            "active": Value::Null,
        });
        let document: Document = serde_json::from_value(value).unwrap();
        assert_eq!(document.grid, None);
        let stored = serde_json::to_value(&document).unwrap();
        assert!(stored.get("grid").is_none());
        let grid: GridSettings =
            serde_json::from_value(serde_json::json!({"spacing": 12.0})).unwrap();
        assert_eq!(grid.spacing, 12.0);
        assert_eq!(grid.subdivisions, DEFAULT_GRID_SUBDIVISIONS);
    }
}
