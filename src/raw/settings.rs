use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::document::Point;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WhiteBalance {
    #[default]
    AsShot,
    Temperature,
    Custom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverlayKind {
    #[default]
    Linear,
    Radial,
    Brush,
}

/// Overlay coordinates refer to the uncropped, oriented image, in 0..1 units.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overlay {
    pub name: String,
    pub enabled: bool,
    pub kind: OverlayKind,
    pub start: Point,
    pub end: Point,
    pub radius: f32,
    pub feather: f32,
    pub invert: bool,
    pub points: Vec<Point>,
    pub exposure: f32,
    pub warmth: f32,
    pub saturation: f32,
}

impl Default for Overlay {
    fn default() -> Self {
        Self {
            name: "Local adjustment".into(),
            enabled: true,
            kind: OverlayKind::Linear,
            start: Point::new(0.5, 0.2),
            end: Point::new(0.5, 0.7),
            radius: 0.08,
            feather: 0.7,
            invert: false,
            points: Vec::new(),
            exposure: 0.0,
            warmth: 0.0,
            saturation: 0.0,
        }
    }
}

/// One colour grading wheel: a hue and saturation that tint a tonal region, and
/// a luminance shift laid over it. Compositor 1.2.3's Color Grading, ported with
/// the same ranges the panel's sliders use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GradeWheel {
    /// The hue the tint is taken from, 0-360 degrees.
    #[serde(default)]
    pub hue: f32,
    /// How much of that hue is mixed in, 0-100.
    #[serde(default)]
    pub saturation: f32,
    /// How far the region's brightness moves, -100-100.
    #[serde(default)]
    pub luminance: f32,
}

impl GradeWheel {
    /// Whether this wheel asks for any change at all.
    pub fn adjusts(&self) -> bool {
        self.saturation > 0.0 || self.luminance != 0.0
    }

    fn validate(&self) -> Result<()> {
        range(self.hue, 0.0, 360.0)?;
        range(self.saturation, 0.0, 100.0)?;
        range(self.luminance, -100.0, 100.0)
    }
}

/// The four grading wheels, plus how far the three tonal ones reach over each
/// other and which end of the range they favour.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grading {
    #[serde(default)]
    pub shadows: GradeWheel,
    #[serde(default)]
    pub midtones: GradeWheel,
    #[serde(default)]
    pub highlights: GradeWheel,
    #[serde(default)]
    pub global: GradeWheel,
    /// How much the three tonal wheels overlap, 0-100.
    #[serde(default = "default_blending")]
    pub blending: f32,
    /// Which end the tonal wheels favour, -100-100; positive favours highlights.
    #[serde(default)]
    pub balance: f32,
}

/// The blending that makes the three tonal wheels meet in the middle, which is
/// not a zero: each tonal wheel has to reach its neighbours to grade the range
/// it is given.
const DEFAULT_BLENDING: f32 = 50.0;

fn default_blending() -> f32 {
    DEFAULT_BLENDING
}

impl Default for Grading {
    fn default() -> Self {
        Self {
            shadows: GradeWheel::default(),
            midtones: GradeWheel::default(),
            highlights: GradeWheel::default(),
            global: GradeWheel::default(),
            blending: DEFAULT_BLENDING,
            balance: 0.0,
        }
    }
}

impl Grading {
    /// The four wheels in the order the pipeline applies them: the three tonal
    /// ones first, then the global one.
    pub fn wheels(&self) -> [GradeWheel; 4] {
        [self.shadows, self.midtones, self.highlights, self.global]
    }

    /// Whether any wheel asks for a change, which is what a project format has
    /// to know: a camera file is developed again when a project is opened, so
    /// dropping the grading would change the picture.
    pub fn adjusts(&self) -> bool {
        self.wheels().iter().any(GradeWheel::adjusts)
    }

    fn validate(&self) -> Result<()> {
        for wheel in self.wheels() {
            wheel.validate()?;
        }
        range(self.blending, 0.0, 100.0)?;
        range(self.balance, -100.0, 100.0)
    }
}

/// Glow's three looks. Warmth tints Diffusion and Bloom from cool to warm;
/// Halation's fringe stays red and warmth only pushes it further that way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GlowStyle {
    #[default]
    Diffusion,
    Bloom,
    Halation,
}

impl GlowStyle {
    /// The value Compositor's kernel switches on.
    pub fn kernel_value(self) -> u32 {
        match self {
            GlowStyle::Diffusion => 0,
            GlowStyle::Bloom => 1,
            GlowStyle::Halation => 2,
        }
    }

    /// The name the panel shows, which is Compositor's own.
    pub fn name(self) -> &'static str {
        match self {
            GlowStyle::Diffusion => "Diffusion",
            GlowStyle::Bloom => "Bloom",
            GlowStyle::Halation => "Halation",
        }
    }

    pub const ALL: [GlowStyle; 3] = [GlowStyle::Diffusion, GlowStyle::Bloom, GlowStyle::Halation];
}

/// The post-crop vignette's three looks. Highlight Priority is the one whose
/// Highlights slider protects bright pixels. Paint Overlay is the plain
/// vignette here: the colour it is named for belongs to Compositor's standalone
/// Vignette filter, which is driven by its own function.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VignetteStyle {
    #[default]
    HighlightPriority,
    ColorPriority,
    PaintOverlay,
}

impl VignetteStyle {
    pub fn kernel_value(self) -> u32 {
        match self {
            VignetteStyle::HighlightPriority => 0,
            VignetteStyle::ColorPriority => 1,
            VignetteStyle::PaintOverlay => 2,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            VignetteStyle::HighlightPriority => "Highlight Priority",
            VignetteStyle::ColorPriority => "Color Priority",
            VignetteStyle::PaintOverlay => "Paint Overlay",
        }
    }

    pub const ALL: [VignetteStyle; 3] = [
        VignetteStyle::HighlightPriority,
        VignetteStyle::ColorPriority,
        VignetteStyle::PaintOverlay,
    ];
}

/// How hard the calibration sliders are allowed to push. Compositor keeps the
/// older processes so a look can be matched; version 6 is its current default
/// and the only one that applies the sliders at full strength.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProcessVersion {
    Version1,
    Version2,
    Version3,
    Version4,
    Version5,
    #[default]
    Version6,
}

impl ProcessVersion {
    /// Compositor counts from one, and its kernel treats anything below one the
    /// same as version one.
    pub fn kernel_value(self) -> u32 {
        match self {
            ProcessVersion::Version1 => 1,
            ProcessVersion::Version2 => 2,
            ProcessVersion::Version3 => 3,
            ProcessVersion::Version4 => 4,
            ProcessVersion::Version5 => 5,
            ProcessVersion::Version6 => 6,
        }
    }

    /// How strongly the process scales the calibration it is given: the older
    /// processes are a gentler correction of the same sliders.
    pub fn scale(self) -> f32 {
        match self {
            ProcessVersion::Version1 => 0.55,
            ProcessVersion::Version2 => 0.65,
            ProcessVersion::Version3 => 0.75,
            ProcessVersion::Version4 => 0.85,
            ProcessVersion::Version5 => 0.92,
            ProcessVersion::Version6 => 1.0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ProcessVersion::Version1 => "Version 1",
            ProcessVersion::Version2 => "Version 2",
            ProcessVersion::Version3 => "Version 3",
            ProcessVersion::Version4 => "Version 4",
            ProcessVersion::Version5 => "Version 5",
            ProcessVersion::Version6 => "Version 6",
        }
    }

    /// What Compositor says about each process, shown under the picker.
    pub fn summary(self) -> &'static str {
        match self {
            ProcessVersion::Version1 => {
                "The first process, and the gentlest. The calibration sliders below apply at 55%."
            }
            ProcessVersion::Version2 => "The calibration sliders below apply at 65%.",
            ProcessVersion::Version3 => "The calibration sliders below apply at 75%.",
            ProcessVersion::Version4 => "The calibration sliders below apply at 85%.",
            ProcessVersion::Version5 => "The calibration sliders below apply at 92%.",
            ProcessVersion::Version6 => {
                "Current default. The calibration sliders below apply at full strength."
            }
        }
    }

    pub const ALL: [ProcessVersion; 6] = [
        ProcessVersion::Version1,
        ProcessVersion::Version2,
        ProcessVersion::Version3,
        ProcessVersion::Version4,
        ProcessVersion::Version5,
        ProcessVersion::Version6,
    ];
}

/// Calibration: one shadow tint and a hue and saturation for each of the three
/// primaries, all in the same −100…100 the panel's sliders use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    #[serde(default)]
    pub process: ProcessVersion,
    /// Rotates the hue of everything darker than a fixed lightness.
    #[serde(default)]
    pub shadow_tint: f32,
    #[serde(default)]
    pub red_hue: f32,
    #[serde(default)]
    pub red_saturation: f32,
    #[serde(default)]
    pub green_hue: f32,
    #[serde(default)]
    pub green_saturation: f32,
    #[serde(default)]
    pub blue_hue: f32,
    #[serde(default)]
    pub blue_saturation: f32,
}

impl Calibration {
    /// Whether any slider asks for a change. The process on its own does not:
    /// it only decides how hard the sliders push.
    pub fn adjusts(&self) -> bool {
        self.sliders().iter().any(|value| *value != 0.0)
    }

    pub fn sliders(&self) -> [f32; 7] {
        [
            self.shadow_tint,
            self.red_hue,
            self.red_saturation,
            self.green_hue,
            self.green_saturation,
            self.blue_hue,
            self.blue_saturation,
        ]
    }

    fn validate(&self) -> Result<()> {
        for value in self.sliders() {
            range(value, -100.0, 100.0)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DevelopSettings {
    pub white_balance: WhiteBalance,
    pub temperature: f32,
    pub tint: f32,
    pub custom_wb: [f32; 3],
    pub exposure: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub clarity: f32,
    pub texture: f32,
    pub dehaze: f32,
    /// Master, red, green, blue; five evenly spaced curve knots each.
    pub curves: [[f32; 5]; 4],
    /// Eight hue bands, each containing hue shift, saturation and lightness.
    pub hsl: [[f32; 3]; 8],
    pub monochrome: bool,
    pub bw_mix: [f32; 3],
    /// Hue (degrees), saturation (percent).
    pub shadow_tone: [f32; 2],
    pub highlight_tone: [f32; 2],
    pub tone_balance: f32,
    /// The colour grading wheels, applied after the tone curve and the mixer.
    pub grading: Grading,
    /// Glow, from the bright areas outwards, and its three looks.
    pub glow: f32,
    pub glow_style: GlowStyle,
    /// How bright an area has to be to glow.
    pub glow_range: f32,
    /// How far the glow reaches.
    pub glow_spread: f32,
    /// From cool to warm; Halation stays red.
    pub glow_warmth: f32,
    /// The post-crop vignette. `vignette` below is the lens correction, which
    /// runs later and is a different control.
    pub vignette_amount: f32,
    pub vignette_style: VignetteStyle,
    pub vignette_midpoint: f32,
    pub vignette_roundness: f32,
    pub vignette_feather: f32,
    /// Protects bright pixels while a dark vignette is applied.
    pub vignette_highlights: f32,
    pub grain_amount: f32,
    pub grain_size: f32,
    pub grain_roughness: f32,
    /// Applied before the light pass, in the same encoded values the rest of
    /// the creative work uses.
    pub calibration: Calibration,
    pub luminance_noise: f32,
    pub color_noise: f32,
    pub sharpen: f32,
    pub sharpen_radius: f32,
    pub sharpen_threshold: f32,
    pub distortion: f32,
    pub chromatic_red: f32,
    pub chromatic_blue: f32,
    pub defringe: f32,
    pub vignette: f32,
    pub rotation: f32,
    pub perspective: [f32; 2],
    /// Left, top, right, bottom in normalized oriented image coordinates.
    pub crop: [f32; 4],
    pub overlays: Vec<Overlay>,
}

impl Default for DevelopSettings {
    fn default() -> Self {
        Self {
            white_balance: WhiteBalance::AsShot,
            temperature: 6500.0,
            tint: 0.0,
            custom_wb: [1.0; 3],
            exposure: 0.0,
            brightness: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            saturation: 0.0,
            vibrance: 0.0,
            clarity: 0.0,
            texture: 0.0,
            dehaze: 0.0,
            curves: [[0.0, 0.25, 0.5, 0.75, 1.0]; 4],
            hsl: [[0.0; 3]; 8],
            monochrome: false,
            bw_mix: [0.2126, 0.7152, 0.0722],
            shadow_tone: [220.0, 0.0],
            highlight_tone: [45.0, 0.0],
            tone_balance: 0.0,
            grading: Grading::default(),
            glow: 0.0,
            glow_style: GlowStyle::default(),
            glow_range: 0.0,
            glow_spread: 0.0,
            glow_warmth: 0.0,
            vignette_amount: 0.0,
            vignette_style: VignetteStyle::default(),
            vignette_midpoint: 50.0,
            vignette_roundness: 0.0,
            vignette_feather: 50.0,
            vignette_highlights: 0.0,
            grain_amount: 0.0,
            grain_size: 25.0,
            grain_roughness: 50.0,
            calibration: Calibration::default(),
            luminance_noise: 0.0,
            color_noise: 20.0,
            sharpen: 25.0,
            sharpen_radius: 1.0,
            sharpen_threshold: 0.01,
            distortion: 0.0,
            chromatic_red: 0.0,
            chromatic_blue: 0.0,
            defringe: 0.0,
            vignette: 0.0,
            rotation: 0.0,
            perspective: [0.0; 2],
            crop: [0.0, 0.0, 1.0, 1.0],
            overlays: Vec::new(),
        }
    }
}

fn range(value: f32, min: f32, max: f32) -> Result<()> {
    ensure!(
        value.is_finite() && (min..=max).contains(&value),
        "Invalid RAW development setting"
    );
    Ok(())
}

impl DevelopSettings {
    pub fn validate(&self) -> Result<()> {
        range(self.temperature, 2000.0, 25_000.0)?;
        range(self.tint, -150.0, 150.0)?;
        range(self.exposure, -10.0, 10.0)?;
        for value in self.custom_wb {
            range(value, 0.01, 100.0)?;
        }
        for value in [
            self.brightness,
            self.contrast,
            self.highlights,
            self.shadows,
            self.whites,
            self.blacks,
            self.saturation,
            self.vibrance,
            self.clarity,
            self.texture,
            self.dehaze,
            self.tone_balance,
            self.distortion,
            self.chromatic_red,
            self.chromatic_blue,
            self.vignette,
        ] {
            range(value, -100.0, 100.0)?;
        }
        for value in [
            self.luminance_noise,
            self.color_noise,
            self.defringe,
            self.glow,
            self.vignette_midpoint,
            self.vignette_feather,
            self.vignette_highlights,
            self.grain_amount,
            self.grain_size,
            self.grain_roughness,
        ] {
            range(value, 0.0, 100.0)?;
        }
        for value in [
            self.glow_range,
            self.glow_spread,
            self.glow_warmth,
            self.vignette_amount,
            self.vignette_roundness,
        ] {
            range(value, -100.0, 100.0)?;
        }
        range(self.sharpen, 0.0, 200.0)?;
        range(self.sharpen_radius, 0.3, 5.0)?;
        range(self.sharpen_threshold, 0.0, 1.0)?;
        range(self.rotation, -45.0, 45.0)?;
        for value in self.perspective {
            range(value, -100.0, 100.0)?;
        }
        for value in self.curves.iter().flatten() {
            range(*value, 0.0, 1.0)?;
        }
        for value in self.hsl.iter().flatten() {
            range(*value, -100.0, 100.0)?;
        }
        for value in self.bw_mix {
            range(value, -1.0, 2.0)?;
        }
        for tone in [self.shadow_tone, self.highlight_tone] {
            range(tone[0], 0.0, 360.0)?;
            range(tone[1], 0.0, 100.0)?;
        }
        for value in self.crop {
            range(value, 0.0, 1.0)?;
        }
        ensure!(
            self.crop[2] - self.crop[0] >= 0.01 && self.crop[3] - self.crop[1] >= 0.01,
            "RAW crop must retain at least 1% of each dimension"
        );
        ensure!(
            self.overlays.len() <= 32,
            "Too many RAW overlays (maximum 32)"
        );
        let mut points = 0;
        for overlay in &self.overlays {
            ensure!(overlay.name.len() <= 256, "RAW overlay name is too long");
            for point in [overlay.start, overlay.end].iter().chain(&overlay.points) {
                range(point.x, 0.0, 1.0)?;
                range(point.y, 0.0, 1.0)?;
            }
            points += overlay.points.len();
            range(overlay.radius, 0.001, 1.0)?;
            range(overlay.feather, 0.01, 1.0)?;
            range(overlay.exposure, -10.0, 10.0)?;
            range(overlay.warmth, -100.0, 100.0)?;
            range(overlay.saturation, -100.0, 100.0)?;
        }
        ensure!(points <= 8192, "Too many RAW brush points (maximum 8192)");
        self.grading.validate()?;
        self.calibration.validate()?;
        Ok(())
    }

    /// Whether the post-grade effects ask for anything: glow, the post-crop
    /// vignette, or grain. A project format has to know, because a camera layer
    /// is developed again when a project opens.
    pub fn adjusts_effects(&self) -> bool {
        self.glow > 0.0
            || self.vignette_amount != 0.0
            || self.grain_amount > 0.0
            || self.calibration.adjusts()
    }
}
