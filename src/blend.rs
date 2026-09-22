use serde::{Deserialize, Serialize};

/// The modes Compositor 1.2.2 offers, in Photoshop's order. `groups` records the menu grouping
/// upstream keeps, and serde stores variant names, so the order here only drives the menu and the
/// GPU mode index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    #[default]
    Normal,
    Darken,
    Multiply,
    ColorBurn,
    LinearBurn,
    Lighten,
    Screen,
    ColorDodge,
    LinearDodge,
    Overlay,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    pub const ALL: [Self; 24] = [
        Self::Normal,
        Self::Darken,
        Self::Multiply,
        Self::ColorBurn,
        Self::LinearBurn,
        Self::Lighten,
        Self::Screen,
        Self::ColorDodge,
        Self::LinearDodge,
        Self::Overlay,
        Self::SoftLight,
        Self::HardLight,
        Self::VividLight,
        Self::LinearLight,
        Self::PinLight,
        Self::HardMix,
        Self::Difference,
        Self::Exclusion,
        Self::Subtract,
        Self::Divide,
        Self::Hue,
        Self::Saturation,
        Self::Color,
        Self::Luminosity,
    ];

    /// Photoshop's grouping, which upstream mirrors: darkening modes together, then lightening,
    /// then the contrast ones, then the comparative ones, then the component modes. The blend menu
    /// draws a line between each group.
    pub fn groups() -> &'static [&'static [Self]] {
        &[
            &[Self::Normal],
            &[
                Self::Darken,
                Self::Multiply,
                Self::ColorBurn,
                Self::LinearBurn,
            ],
            &[
                Self::Lighten,
                Self::Screen,
                Self::ColorDodge,
                Self::LinearDodge,
            ],
            &[
                Self::Overlay,
                Self::SoftLight,
                Self::HardLight,
                Self::VividLight,
                Self::LinearLight,
                Self::PinLight,
                Self::HardMix,
            ],
            &[
                Self::Difference,
                Self::Exclusion,
                Self::Subtract,
                Self::Divide,
            ],
            &[Self::Hue, Self::Saturation, Self::Color, Self::Luminosity],
        ]
    }

    /// The names Compositor stores in projects; they must match `LayerBlendMode` exactly.
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Darken => "Darken",
            Self::Multiply => "Multiply",
            Self::ColorBurn => "Color Burn",
            Self::LinearBurn => "Linear Burn",
            Self::Lighten => "Lighten",
            Self::Screen => "Screen",
            Self::ColorDodge => "Color Dodge",
            Self::LinearDodge => "Linear Dodge (Add)",
            Self::Overlay => "Overlay",
            Self::SoftLight => "Soft Light",
            Self::HardLight => "Hard Light",
            Self::VividLight => "Vivid Light",
            Self::LinearLight => "Linear Light",
            Self::PinLight => "Pin Light",
            Self::HardMix => "Hard Mix",
            Self::Difference => "Difference",
            Self::Exclusion => "Exclusion",
            Self::Subtract => "Subtract",
            Self::Divide => "Divide",
            Self::Hue => "Hue",
            Self::Saturation => "Saturation",
            Self::Color => "Color",
            Self::Luminosity => "Luminosity",
        }
    }
}

fn luminance(c: [f32; 3]) -> f32 {
    c[0] * 0.3 + c[1] * 0.59 + c[2] * 0.11
}
fn saturation(c: [f32; 3]) -> f32 {
    c.into_iter().fold(f32::MIN, f32::max) - c.into_iter().fold(f32::MAX, f32::min)
}

fn set_luminance(mut c: [f32; 3], value: f32) -> [f32; 3] {
    let delta = value - luminance(c);
    c = c.map(|v| v + delta);
    let min = c.into_iter().fold(f32::MAX, f32::min);
    let max = c.into_iter().fold(f32::MIN, f32::max);
    if min < 0.0 {
        c = c.map(|v| value + (v - value) * value / (value - min).max(1e-6));
    }
    if max > 1.0 {
        c = c.map(|v| value + (v - value) * (1.0 - value) / (max - value).max(1e-6));
    }
    c
}

fn set_saturation(c: [f32; 3], value: f32) -> [f32; 3] {
    let min = c.into_iter().fold(f32::MAX, f32::min);
    let max = c.into_iter().fold(f32::MIN, f32::max);
    if max <= min {
        [0.0; 3]
    } else {
        c.map(|v| (v - min) * value / (max - min))
    }
}

fn color_dodge(d: f32, s: f32) -> f32 {
    if d <= 0.0 {
        0.0
    } else if s >= 1.0 {
        1.0
    } else {
        (d / (1.0 - s)).min(1.0)
    }
}

fn color_burn(d: f32, s: f32) -> f32 {
    if d >= 1.0 {
        1.0
    } else if s <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - d) / s).min(1.0)
    }
}

/// Vivid Light burns or dodges according to the source, with twice its distance from mid grey.
fn vivid_light(d: f32, s: f32) -> f32 {
    if s <= 0.5 {
        color_burn(d, 2.0 * s)
    } else {
        color_dodge(d, 2.0 * s - 1.0)
    }
}

/// W3C compositing formula, with straight sRGB inputs and outputs. The ten modes Compositor 1.2.2
/// added are Photoshop's definitions, computed in the same sRGB the canvas is in.
pub fn composite(dst: [f32; 4], src: [f32; 4], mode: BlendMode) -> [f32; 4] {
    let alpha = src[3] + dst[3] * (1.0 - src[3]);
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    let d = [dst[0], dst[1], dst[2]];
    let s = [src[0], src[1], src[2]];
    let mixed = match mode {
        BlendMode::Hue => set_luminance(set_saturation(s, saturation(d)), luminance(d)),
        BlendMode::Saturation => set_luminance(set_saturation(d, saturation(s)), luminance(d)),
        BlendMode::Color => set_luminance(s, luminance(d)),
        BlendMode::Luminosity => set_luminance(d, luminance(s)),
        _ => std::array::from_fn(|i| match mode {
            BlendMode::Darken => d[i].min(s[i]),
            BlendMode::Multiply => d[i] * s[i],
            BlendMode::ColorBurn => color_burn(d[i], s[i]),
            BlendMode::LinearBurn => (d[i] + s[i] - 1.0).max(0.0),
            BlendMode::Lighten => d[i].max(s[i]),
            BlendMode::Screen => d[i] + s[i] - d[i] * s[i],
            BlendMode::ColorDodge => color_dodge(d[i], s[i]),
            BlendMode::LinearDodge => (d[i] + s[i]).min(1.0),
            BlendMode::Overlay => {
                if d[i] <= 0.5 {
                    2.0 * d[i] * s[i]
                } else {
                    1.0 - 2.0 * (1.0 - d[i]) * (1.0 - s[i])
                }
            }
            BlendMode::SoftLight => {
                if s[i] <= 0.5 {
                    d[i] - (1.0 - 2.0 * s[i]) * d[i] * (1.0 - d[i])
                } else {
                    let soft = if d[i] <= 0.25 {
                        ((16.0 * d[i] - 12.0) * d[i] + 4.0) * d[i]
                    } else {
                        d[i].sqrt()
                    };
                    d[i] + (2.0 * s[i] - 1.0) * (soft - d[i])
                }
            }
            BlendMode::HardLight => {
                if s[i] <= 0.5 {
                    2.0 * d[i] * s[i]
                } else {
                    1.0 - 2.0 * (1.0 - d[i]) * (1.0 - s[i])
                }
            }
            BlendMode::VividLight => vivid_light(d[i], s[i]),
            BlendMode::LinearLight => (d[i] + 2.0 * s[i] - 1.0).clamp(0.0, 1.0),
            BlendMode::PinLight => {
                if s[i] <= 0.5 {
                    d[i].min(2.0 * s[i])
                } else {
                    d[i].max(2.0 * s[i] - 1.0)
                }
            }
            BlendMode::HardMix => {
                if vivid_light(d[i], s[i]) < 0.5 {
                    0.0
                } else {
                    1.0
                }
            }
            BlendMode::Difference => (d[i] - s[i]).abs(),
            BlendMode::Exclusion => d[i] + s[i] - 2.0 * d[i] * s[i],
            BlendMode::Subtract => (d[i] - s[i]).max(0.0),
            BlendMode::Divide => {
                if s[i] <= 0.0 {
                    1.0
                } else {
                    (d[i] / s[i]).min(1.0)
                }
            }
            _ => s[i],
        }),
    };
    let mut result = [0.0; 4];
    for i in 0..3 {
        result[i] = ((1.0 - src[3]) * dst[3] * dst[i]
            + (1.0 - dst[3]) * src[3] * src[i]
            + dst[3] * src[3] * mixed[i])
            / alpha;
    }
    result[3] = alpha;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blended(dst: f32, src: f32, mode: BlendMode) -> f32 {
        composite([dst, dst, dst, 1.0], [src, src, src, 1.0], mode)[0]
    }

    fn close(value: f32, expected: f32) {
        assert!(
            (value - expected).abs() <= 1e-5,
            "expected {expected}, got {value}"
        );
    }

    #[test]
    fn alpha_over_and_blend_on_transparency() {
        let result = composite(
            [0.0, 0.0, 1.0, 1.0],
            [1.0, 0.0, 0.0, 0.5],
            BlendMode::Normal,
        );
        assert_eq!(result, [0.5, 0.0, 0.5, 1.0]);
        for mode in BlendMode::ALL {
            let result = composite([0.0; 4], [0.2, 0.4, 0.8, 0.5], mode);
            assert_eq!(result, [0.2, 0.4, 0.8, 0.5]);
        }
    }

    #[test]
    fn menu_groups_flatten_to_the_mode_list() {
        let flattened: Vec<BlendMode> = BlendMode::groups()
            .iter()
            .flat_map(|group| group.iter().copied())
            .collect();
        assert_eq!(flattened, BlendMode::ALL.to_vec());
    }

    #[test]
    fn photoshop_names_match_compositor() {
        // The exact strings Compositor writes into projects and PSD imports.
        let names: Vec<&str> = BlendMode::ALL.iter().map(|mode| mode.name()).collect();
        assert_eq!(
            names,
            [
                "Normal",
                "Darken",
                "Multiply",
                "Color Burn",
                "Linear Burn",
                "Lighten",
                "Screen",
                "Color Dodge",
                "Linear Dodge (Add)",
                "Overlay",
                "Soft Light",
                "Hard Light",
                "Vivid Light",
                "Linear Light",
                "Pin Light",
                "Hard Mix",
                "Difference",
                "Exclusion",
                "Subtract",
                "Divide",
                "Hue",
                "Saturation",
                "Color",
                "Luminosity",
            ]
        );
    }

    #[test]
    fn extended_modes_follow_photoshop() {
        close(blended(0.4, 0.8, BlendMode::LinearBurn), 0.2);
        close(blended(0.4, 0.8, BlendMode::LinearDodge), 1.0);
        close(blended(0.4, 0.3, BlendMode::HardLight), 0.24);
        close(blended(0.4, 0.8, BlendMode::HardLight), 0.76);
        close(blended(0.4, 0.3, BlendMode::VividLight), 0.0);
        close(blended(0.4, 0.8, BlendMode::VividLight), 1.0);
        close(blended(0.4, 0.2, BlendMode::LinearLight), 0.0);
        close(blended(0.4, 0.8, BlendMode::LinearLight), 1.0);
        close(blended(0.4, 0.3, BlendMode::PinLight), 0.4);
        close(blended(0.4, 0.8, BlendMode::PinLight), 0.6);
        close(blended(0.4, 0.3, BlendMode::HardMix), 0.0);
        close(blended(0.4, 0.8, BlendMode::HardMix), 1.0);
        close(blended(0.4, 0.8, BlendMode::Exclusion), 0.56);
        close(blended(0.4, 0.8, BlendMode::Subtract), 0.0);
        close(blended(0.8, 0.3, BlendMode::Subtract), 0.5);
        close(blended(0.4, 0.8, BlendMode::Divide), 0.5);
        close(blended(0.4, 0.0, BlendMode::Divide), 1.0);
    }

    #[test]
    fn extended_modes_keep_color_dodge_and_burn_edges() {
        // Compositor's Color Burn and Color Dodge clamp the same way, at zero and one.
        close(blended(0.0, 1.0, BlendMode::VividLight), 0.0);
        close(blended(1.0, 0.59, BlendMode::VividLight), 1.0);
        close(blended(1.0, 0.0, BlendMode::VividLight), 1.0);
        // Pin Light takes the darker or lighter side of the doubled source, so it can darken too.
        close(blended(1.0, 0.2, BlendMode::PinLight), 0.4);
        close(blended(0.0, 0.8, BlendMode::PinLight), 0.6);
    }
}
