use std::{
    f32::consts::TAU,
    fs::File,
    io::{Read, Write},
    ops::RangeInclusive,
};

use egui::{Color32, Sense, Stroke, pos2, vec2};
use mectov::raw::{self, DevelopSettings, GradeWheel, Grading, Overlay, OverlayKind, WhiteBalance};

use super::{develop::Develop, theme, widgets};

fn slider(ui: &mut egui::Ui, label: &str, value: &mut f32, range: RangeInclusive<f32>) {
    slider_to(ui, label, value, range, 0.0);
}

/// A Develop slider, for the controls whose neutral is not zero: the range has
/// to hold the value, or there is nothing a double-click could reset to.
fn slider_to(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    reset: f32,
) {
    let reset = (range.contains(&reset)).then_some(reset);
    ui.horizontal(|ui| {
        ui.add_sized([105.0, 20.0], egui::Label::new(label));
        let mut response = ui.add(
            egui::Slider::new(value, range)
                .clamping(egui::SliderClamping::Edits)
                .max_decimals(2),
        );
        if let Some(reset) = reset {
            super::widgets::reset_on_double_click(ui, &mut response, value, f64::from(reset));
        }
    });
}

fn percent(ui: &mut egui::Ui, label: &str, value: &mut f32) {
    slider(ui, label, value, -100.0..=100.0);
}

pub(super) fn controls(ui: &mut egui::Ui, d: &mut Develop) {
    histogram(ui, d);
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!d.undo.is_empty(), egui::Button::new("Undo"))
            .clicked()
        {
            d.undo(false);
        }
        if ui
            .add_enabled(!d.redo.is_empty(), egui::Button::new("Redo"))
            .clicked()
        {
            d.undo(true);
        }
        if ui
            .button("Reset")
            .on_hover_text("Reset all Develop adjustments to defaults")
            .clicked()
        {
            d.settings = DevelopSettings::default();
            d.selected_overlay = None;
        }
        ui.menu_button("Presets", |ui| {
            for (name, preset) in [
                ("Natural", DevelopSettings::default()),
                (
                    "Landscape",
                    DevelopSettings {
                        contrast: 12.0,
                        highlights: -30.0,
                        shadows: 20.0,
                        vibrance: 20.0,
                        clarity: 12.0,
                        ..Default::default()
                    },
                ),
                (
                    "Black & white",
                    DevelopSettings {
                        monochrome: true,
                        contrast: 18.0,
                        ..Default::default()
                    },
                ),
            ] {
                if ui.button(name).clicked() {
                    d.settings = preset;
                    d.selected_overlay = None;
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Save settings…").clicked() {
                save_preset(d);
                ui.close();
            }
            if ui.button("Load settings…").clicked() {
                load_preset(d);
                ui.close();
            }
        });
    });
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        for (index, name) in ["Basic", "Tone", "Detail", "Lens", "Masks", "Info"]
            .into_iter()
            .enumerate()
        {
            ui.selectable_value(&mut d.panel, index, name);
        }
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("raw_settings")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(4.0);
            match d.panel {
                0 => basic(ui, d),
                1 => tones(ui, d),
                2 => detail(ui, d),
                3 => lens(ui, d),
                4 => masks(ui, d),
                _ => metadata(ui, d),
            }
            ui.add_space(16.0);
        });
}

fn heading(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.strong(title);
    ui.add_space(6.0);
}

fn basic(ui: &mut egui::Ui, d: &mut Develop) {
    heading(ui, "White balance");
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("raw_wb")
            .selected_text(match d.settings.white_balance {
                WhiteBalance::AsShot => "As shot",
                WhiteBalance::Temperature => "Temperature",
                WhiteBalance::Custom => "Sampled neutral",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut d.settings.white_balance,
                    WhiteBalance::AsShot,
                    "As shot",
                );
                ui.selectable_value(
                    &mut d.settings.white_balance,
                    WhiteBalance::Temperature,
                    "Temperature",
                );
                for (name, kelvin) in [
                    ("Daylight", 5500.0),
                    ("Cloudy", 6500.0),
                    ("Shade", 7500.0),
                    ("Tungsten", 2850.0),
                    ("Flash", 6000.0),
                ] {
                    if ui.button(name).clicked() {
                        d.settings.white_balance = WhiteBalance::Temperature;
                        d.settings.temperature = kelvin;
                        d.settings.tint = 0.0;
                        ui.close();
                    }
                }
            });
        if ui.selectable_label(d.picker, "Pick neutral").clicked() {
            d.picker = !d.picker;
            d.draw_overlay = false;
        }
    });
    ui.add_enabled_ui(
        d.settings.white_balance == WhiteBalance::Temperature,
        |ui| {
            slider(
                ui,
                "Temperature K",
                &mut d.settings.temperature,
                2000.0..=25_000.0,
            );
        },
    );
    slider(ui, "Tint", &mut d.settings.tint, -150.0..=150.0);
    heading(ui, "Light");
    if ui.button("Auto exposure").clicked()
        && let Some(raw) = &d.proxy
    {
        d.settings.exposure = raw::auto_exposure(raw);
    }
    slider(ui, "Exposure EV", &mut d.settings.exposure, -10.0..=10.0);
    percent(ui, "Brightness", &mut d.settings.brightness);
    percent(ui, "Contrast", &mut d.settings.contrast);
    percent(ui, "Highlights", &mut d.settings.highlights);
    percent(ui, "Shadows", &mut d.settings.shadows);
    percent(ui, "Whites", &mut d.settings.whites);
    percent(ui, "Blacks", &mut d.settings.blacks);
    heading(ui, "Presence");
    percent(ui, "Clarity", &mut d.settings.clarity);
    percent(ui, "Texture", &mut d.settings.texture);
    percent(ui, "Dehaze", &mut d.settings.dehaze);
    percent(ui, "Vibrance", &mut d.settings.vibrance);
    percent(ui, "Saturation", &mut d.settings.saturation);
}

fn tones(ui: &mut egui::Ui, d: &mut Develop) {
    heading(ui, "Tone curve");
    ui.horizontal(|ui| {
        for (i, name) in ["RGB", "Red", "Green", "Blue"].iter().enumerate() {
            ui.selectable_value(&mut d.curve_channel, i, *name);
        }
    });
    curve(ui, &mut d.settings.curves[d.curve_channel], d.curve_channel);
    ui.horizontal(|ui| {
        if ui.button("Linear").clicked() {
            d.settings.curves[d.curve_channel] = [0.0, 0.25, 0.5, 0.75, 1.0];
        }
        if ui.button("S curve").clicked() {
            d.settings.curves[d.curve_channel] = [0.0, 0.18, 0.5, 0.82, 1.0];
        }
        if ui.button("Lift blacks").clicked() {
            d.settings.curves[d.curve_channel] = [0.08, 0.28, 0.5, 0.75, 1.0];
        }
    });
    heading(ui, "Color mixer");
    egui::ComboBox::from_id_salt("raw_hsl")
        .selected_text(
            [
                "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
            ][d.hsl_band],
        )
        .show_ui(ui, |ui| {
            for (i, name) in [
                "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
            ]
            .iter()
            .enumerate()
            {
                ui.selectable_value(&mut d.hsl_band, i, *name);
            }
        });
    percent(ui, "Hue", &mut d.settings.hsl[d.hsl_band][0]);
    percent(ui, "Saturation", &mut d.settings.hsl[d.hsl_band][1]);
    percent(ui, "Lightness", &mut d.settings.hsl[d.hsl_band][2]);
    heading(ui, "Black & white");
    ui.checkbox(&mut d.settings.monochrome, "Monochrome");
    ui.add_enabled_ui(d.settings.monochrome, |ui| {
        for (i, label) in ["Red mix", "Green mix", "Blue mix"].iter().enumerate() {
            slider(ui, label, &mut d.settings.bw_mix[i], -1.0..=2.0);
        }
    });
    heading(ui, "Split toning");
    slider(
        ui,
        "Shadow hue",
        &mut d.settings.shadow_tone[0],
        0.0..=360.0,
    );
    slider(
        ui,
        "Shadow amount",
        &mut d.settings.shadow_tone[1],
        0.0..=100.0,
    );
    slider(
        ui,
        "Highlight hue",
        &mut d.settings.highlight_tone[0],
        0.0..=360.0,
    );
    slider(
        ui,
        "Highlight amount",
        &mut d.settings.highlight_tone[1],
        0.0..=100.0,
    );
    percent(ui, "Balance", &mut d.settings.tone_balance);
    // Open by default, as Compositor 1.3.2 has it: the wheels are the last thing
    // a reader looks for on a page they have developed.
    egui::CollapsingHeader::new("Color grading")
        .default_open(true)
        .show(ui, |ui| {
            grading(ui, &mut d.settings.grading);
        });
}

/// The colour grading wheels, in the order the engine applies them: the three
/// tonal wheels, then the global one, with the blending and balance that decide
/// how far each tonal wheel reaches over the others.
///
/// The Develop panel and the Camera Raw filter both show this, so it draws with
/// the app's own slider and the same double-click reset the rest of the panel
/// uses, and reports whether anything moved so the filter can mark itself
/// edited.
pub(super) fn grading(ui: &mut egui::Ui, grading: &mut Grading) -> bool {
    let mut changed = false;
    // The four wheels share the width the panel has, so the section fits the
    // Develop panel and the narrower filter dialog alike.
    let column =
        ((ui.available_width() - 3.0 * ui.spacing().item_spacing.x) / 4.0).clamp(48.0, 112.0);
    ui.horizontal(|ui| {
        for (name, each) in [
            ("Shadows", &mut grading.shadows),
            ("Midtones", &mut grading.midtones),
            ("Highlights", &mut grading.highlights),
            ("Global", &mut grading.global),
        ] {
            changed |= wheel(ui, name, each, column);
        }
    });
    ui.add_space(4.0);
    changed |= ui
        .add(
            widgets::Slider::new(&mut grading.blending, 0.0..=100.0)
                .percentage()
                .text("Blending"),
        )
        .changed();
    changed |= ui
        .add(
            widgets::Slider::new(&mut grading.balance, -100.0..=100.0)
                .percentage()
                .text("Balance"),
        )
        .changed();
    changed
}

/// The colour one wheel picks, at the strength the puck sits from the centre.
fn wheel_color(hue: f32, saturation: f32) -> Color32 {
    let [r, g, b] = mectov::effects::hsl_to_rgb([hue, saturation, 0.5]);
    Color32::from_rgb(
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

/// One wheel: a disc the puck rides, and the luminance shift that rides with it.
/// The puck's angle is the hue, its distance from the centre the tint's
/// strength, and a double click on the disc returns the wheel to neutral, the
/// same gesture the sliders use.
fn wheel(ui: &mut egui::Ui, name: &str, wheel: &mut GradeWheel, column: f32) -> bool {
    ui.vertical(|ui| {
        // A plain label sizes itself to the name, so a column stays as narrow as
        // the disc rather than taking the whole row.
        ui.label(name);
        let size = (column - ui.spacing().item_spacing.x).clamp(36.0, 104.0);
        let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
        let center = rect.center();
        let radius = size / 2.0 - 1.0;
        let painter = ui.painter();
        // The disc: hue around the rim, strength outward, so the puck can sit
        // anywhere in it. A neutral disc underneath keeps the middle from
        // reading as a colour of its own.
        const RINGS: usize = 4;
        const SLICES: usize = 24;
        painter.circle_filled(center, radius, theme::FIELD);
        for ring in 0..RINGS {
            let inner = ring as f32 / RINGS as f32;
            let outer = (ring + 1) as f32 / RINGS as f32;
            let saturation = outer * 0.9;
            for slice in 0..SLICES {
                let start = slice as f32 / SLICES as f32 * TAU;
                let end = (slice + 1) as f32 / SLICES as f32 * TAU;
                let middle = (start + end) / 2.0;
                let point = |angle: f32, reach: f32| {
                    pos2(
                        center.x + angle.sin() * radius * reach,
                        center.y - angle.cos() * radius * reach,
                    )
                };
                let color = wheel_color(middle.to_degrees().rem_euclid(360.0), saturation);
                let wedge = if ring == 0 {
                    vec![center, point(end, outer), point(start, outer)]
                } else {
                    vec![
                        point(start, inner),
                        point(end, inner),
                        point(end, outer),
                        point(start, outer),
                    ]
                };
                painter.add(egui::Shape::convex_polygon(wedge, color, Stroke::NONE));
            }
        }
        painter.circle_stroke(center, radius, Stroke::new(0.5_f32, theme::DIVIDER));
        let mut changed = false;
        if response.double_clicked() {
            *wheel = GradeWheel::default();
            changed = true;
        } else if (response.dragged() || response.clicked())
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let offset = pointer - center;
            if offset.length() > 0.0 {
                // Hue runs clockwise from the top, the way a colour wheel is
                // read, so red is where a reader expects it.
                wheel.hue = offset.x.atan2(-offset.y).to_degrees().rem_euclid(360.0);
            }
            wheel.saturation = (offset.length() / radius * 100.0).clamp(0.0, 100.0);
            changed = true;
        }
        // The puck: the tint it points at, ringed in white or black so the
        // luminance shift reads at a glance.
        let angle = wheel.hue.to_radians();
        let reach = wheel.saturation / 100.0;
        let puck = pos2(
            center.x + angle.sin() * radius * reach,
            center.y - angle.cos() * radius * reach,
        );
        painter.circle_filled(puck, 4.5, wheel_color(wheel.hue, 0.9));
        painter.circle_stroke(
            puck,
            4.5,
            Stroke::new(
                1.5_f32,
                if wheel.luminance < 0.0 {
                    Color32::BLACK
                } else {
                    Color32::WHITE
                },
            ),
        );
        // The luminance shift rides under its wheel: a bare track rather than a
        // labelled slider, because the column is only as wide as the disc, and a
        // double click returns the shift to neutral like every other slider.
        // egui's slider takes its width from the panel's spacing, which is a
        // whole row wide in the filter dialog, so the column lends it its own.
        let panel_width = ui.spacing_mut().slider_width;
        ui.spacing_mut().slider_width = size;
        let mut response = ui.add_sized(
            [size, 18.0],
            egui::Slider::new(&mut wheel.luminance, -100.0..=100.0)
                .clamping(egui::SliderClamping::Edits)
                .show_value(false),
        );
        ui.spacing_mut().slider_width = panel_width;
        response = response.on_hover_text("Luminance");
        widgets::reset_on_double_click(ui, &mut response, &mut wheel.luminance, 0.0);
        changed | response.changed()
    })
    .inner
}

fn detail(ui: &mut egui::Ui, d: &mut Develop) {
    heading(ui, "Noise reduction");
    slider(
        ui,
        "Luminance",
        &mut d.settings.luminance_noise,
        0.0..=100.0,
    );
    slider(ui, "Color", &mut d.settings.color_noise, 0.0..=100.0);
    heading(ui, "Sharpening");
    slider(ui, "Amount", &mut d.settings.sharpen, 0.0..=200.0);
    slider_to(
        ui,
        "Radius px",
        &mut d.settings.sharpen_radius,
        0.3..=5.0,
        raw::DevelopSettings::default().sharpen_radius,
    );
    slider(
        ui,
        "Threshold",
        &mut d.settings.sharpen_threshold,
        0.0..=0.2,
    );
    ui.add_space(12.0);
    ui.checkbox(&mut d.full_preview, "Full-resolution preview");
    ui.label(egui::RichText::new("Use 100% to judge sharpening and noise reduction. Full-resolution previews take longer to update.").color(theme::MUTED));
}

fn lens(ui: &mut egui::Ui, d: &mut Develop) {
    heading(ui, "Manual lens correction");
    if let Some(asset) = &d.asset
        && !asset.metadata.lens.is_empty()
    {
        ui.label(&asset.metadata.lens);
    }
    percent(ui, "Distortion", &mut d.settings.distortion);
    percent(ui, "Red / cyan", &mut d.settings.chromatic_red);
    percent(ui, "Blue / yellow", &mut d.settings.chromatic_blue);
    slider(ui, "Defringe", &mut d.settings.defringe, 0.0..=100.0);
    percent(ui, "Vignetting", &mut d.settings.vignette);
    heading(ui, "Geometry");
    slider(ui, "Straighten °", &mut d.settings.rotation, -45.0..=45.0);
    percent(ui, "Horizontal", &mut d.settings.perspective[0]);
    percent(ui, "Vertical", &mut d.settings.perspective[1]);
    heading(ui, "Crop");
    ui.label(
        egui::RichText::new("Bounds as a fraction of the original image")
            .small()
            .color(theme::MUTED),
    );
    let [left, top, right, bottom] = d.settings.crop;
    slider(ui, "Left", &mut d.settings.crop[0], 0.0..=(right - 0.01));
    slider(ui, "Top", &mut d.settings.crop[1], 0.0..=(bottom - 0.01));
    slider_to(
        ui,
        "Right",
        &mut d.settings.crop[2],
        (left + 0.01)..=1.0,
        1.0,
    );
    slider_to(
        ui,
        "Bottom",
        &mut d.settings.crop[3],
        (top + 0.01)..=1.0,
        1.0,
    );
    ui.horizontal(|ui| {
        if ui.button("Uncrop").clicked() {
            d.settings.crop = [0.0, 0.0, 1.0, 1.0];
        }
        if ui.button("Square").clicked()
            && let Some(raw) = &d.proxy
        {
            let aspect = raw.camera.width() as f32 / raw.camera.height() as f32;
            d.settings.crop = if aspect >= 1.0 {
                let margin = (1.0 - 1.0 / aspect) * 0.5;
                [margin, 0.0, 1.0 - margin, 1.0]
            } else {
                let margin = (1.0 - aspect) * 0.5;
                [0.0, margin, 1.0, 1.0 - margin]
            };
        }
    });
}

fn masks(ui: &mut egui::Ui, d: &mut Develop) {
    heading(ui, "Local adjustments");
    ui.add_enabled_ui(d.settings.overlays.len() < 32, |ui| {
        ui.horizontal(|ui| {
            for (name, kind) in [
                ("Linear", OverlayKind::Linear),
                ("Radial", OverlayKind::Radial),
                ("Brush", OverlayKind::Brush),
            ] {
                if ui.button(format!("+ {name}")).clicked() {
                    d.settings.overlays.push(Overlay {
                        name: format!("{name} {}", d.settings.overlays.len() + 1),
                        kind,
                        ..Default::default()
                    });
                    d.selected_overlay = Some(d.settings.overlays.len() - 1);
                    d.draw_overlay = true;
                    d.show_mask = true;
                    d.picker = false;
                }
            }
        });
    });
    for (i, overlay) in d.settings.overlays.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut overlay.enabled, "");
            if ui
                .selectable_label(d.selected_overlay == Some(i), &overlay.name)
                .clicked()
            {
                d.selected_overlay = Some(i);
            }
        });
    }
    let Some(index) = d
        .selected_overlay
        .filter(|i| *i < d.settings.overlays.len())
    else {
        ui.label("Add a mask, then drag over the photo to place it.");
        return;
    };
    ui.separator();
    ui.horizontal(|ui| {
        ui.checkbox(&mut d.draw_overlay, "Draw mask");
        ui.checkbox(&mut d.show_mask, "Show guides");
    });
    let overlay = &mut d.settings.overlays[index];
    ui.text_edit_singleline(&mut overlay.name);
    ui.checkbox(&mut overlay.invert, "Invert mask");
    if overlay.kind == OverlayKind::Brush {
        slider_to(
            ui,
            "Brush radius",
            &mut overlay.radius,
            0.005..=0.3,
            raw::Overlay::default().radius,
        );
        if ui.button("Clear brush").clicked() {
            overlay.points.clear();
        }
    }
    if overlay.kind != OverlayKind::Linear {
        slider_to(
            ui,
            "Feather",
            &mut overlay.feather,
            0.01..=1.0,
            raw::Overlay::default().feather,
        );
    }
    slider(ui, "Exposure EV", &mut overlay.exposure, -10.0..=10.0);
    percent(ui, "Warmth", &mut overlay.warmth);
    percent(ui, "Saturation", &mut overlay.saturation);
    if widgets::button(ui, "Delete mask").clicked() {
        d.settings.overlays.remove(index);
        d.selected_overlay = None;
        d.draw_overlay = false;
    }
}

fn metadata(ui: &mut egui::Ui, d: &Develop) {
    let Some(asset) = &d.asset else {
        return;
    };
    let m = &asset.metadata;
    heading(ui, "Camera information");
    ui.strong(&m.camera);
    if !m.lens.is_empty() {
        ui.label(&m.lens);
    }
    ui.add_space(12.0);
    egui::Grid::new("raw_metadata")
        .spacing(vec2(14.0, 9.0))
        .show(ui, |ui| {
            for (label, value) in [
                ("Source", asset.filename.clone()),
                ("Dimensions", format!("{} × {}", m.width, m.height)),
                ("Decoded depth", format!("{} bit", m.bits)),
                ("ISO", m.iso.map_or("—".into(), |v| v.to_string())),
                (
                    "Aperture",
                    m.aperture.map_or("—".into(), |v| format!("f/{v:.1}")),
                ),
                (
                    "Shutter",
                    m.shutter.map_or("—".into(), |v| {
                        if v > 0.0 && v < 1.0 {
                            format!("1/{:.0} s", 1.0 / v)
                        } else {
                            format!("{v:.2} s")
                        }
                    }),
                ),
                (
                    "Focal length",
                    m.focal_length.map_or("—".into(), |v| format!("{v:.0} mm")),
                ),
                (
                    "RAW storage",
                    format!(
                        "Embedded · {:.1} MiB",
                        asset.bytes.len() as f64 / 1_048_576.0
                    ),
                ),
            ] {
                ui.label(egui::RichText::new(label).color(theme::MUTED));
                ui.label(value);
                ui.end_row();
            }
        });
    heading(ui, "Output");
    ui.label("Embedded RAW layer with an sRGB photo render. Double-click the layer to return to Develop.");
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Lens corrections are manual. The photo editor currently uses 8-bit sRGB; RAW data and Develop settings retain their original precision.").color(theme::MUTED));
}

fn histogram(ui: &mut egui::Ui, d: &Develop) {
    ui.horizontal(|ui| {
        ui.strong("Histogram");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new("RGB").small().color(theme::MUTED));
        });
    });
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 84.0), Sense::hover());
    ui.painter().rect_filled(rect, 4.0, theme::FIELD);
    for n in 1..4 {
        let x = rect.left() + rect.width() * n as f32 / 4.0;
        ui.painter().line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(0.5_f32, theme::DIVIDER),
        );
    }
    let max = d
        .histogram
        .iter()
        .flatten()
        .copied()
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    for (c, color) in [
        Color32::from_rgb(235, 90, 98),
        Color32::from_rgb(103, 208, 135),
        Color32::from_rgb(90, 157, 255),
    ]
    .iter()
    .enumerate()
    {
        let points: Vec<_> = d.histogram[c]
            .iter()
            .enumerate()
            .map(|(i, count)| {
                pos2(
                    rect.left() + rect.width() * i as f32 / 255.0,
                    rect.bottom() - (*count as f32 / max).sqrt() * rect.height(),
                )
            })
            .collect();
        ui.painter()
            .add(egui::Shape::line(points, Stroke::new(1.0_f32, *color)));
    }
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("Shadows {:.2}%", d.clipping[0]))
                .small()
                .color(theme::MUTED),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("Highlights {:.2}%", d.clipping[1]))
                    .small()
                    .color(theme::MUTED),
            );
        });
    });
}

pub(super) fn curve(ui: &mut egui::Ui, knots: &mut [f32; 5], channel: usize) {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 175.0), Sense::click_and_drag());
    let rect = rect.shrink(5.0);
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, theme::FIELD);
    for i in 1..4 {
        let t = i as f32 / 4.0;
        painter.line_segment(
            [
                pos2(rect.left() + t * rect.width(), rect.top()),
                pos2(rect.left() + t * rect.width(), rect.bottom()),
            ],
            Stroke::new(0.5_f32, theme::DIVIDER),
        );
        painter.line_segment(
            [
                pos2(rect.left(), rect.top() + t * rect.height()),
                pos2(rect.right(), rect.top() + t * rect.height()),
            ],
            Stroke::new(0.5_f32, theme::DIVIDER),
        );
    }
    painter.line_segment(
        [rect.left_bottom(), rect.right_top()],
        Stroke::new(0.5_f32, theme::MUTED),
    );
    if (response.dragged() || response.clicked())
        && let Some(point) = response.interact_pointer_pos()
    {
        let index = (((point.x - rect.left()) / rect.width()) * 4.0)
            .round()
            .clamp(0.0, 4.0) as usize;
        knots[index] = ((rect.bottom() - point.y) / rect.height()).clamp(0.0, 1.0);
    }
    let points: Vec<_> = knots
        .iter()
        .enumerate()
        .map(|(i, value)| {
            pos2(
                rect.left() + rect.width() * i as f32 / 4.0,
                rect.bottom() - rect.height() * value,
            )
        })
        .collect();
    let color = [
        Color32::WHITE,
        Color32::LIGHT_RED,
        Color32::LIGHT_GREEN,
        Color32::LIGHT_BLUE,
    ][channel];
    painter.add(egui::Shape::line(
        points.clone(),
        Stroke::new(1.5_f32, color),
    ));
    for point in points {
        painter.circle_filled(point, 4.0, color);
    }
}

fn save_preset(d: &mut Develop) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("RAW settings", &["json"])
        .set_file_name("raw-settings.json")
        .save_file()
    else {
        return;
    };
    let result = (|| -> anyhow::Result<()> {
        d.settings.validate()?;
        let parent = path.parent().unwrap_or(std::path::Path::new("."));
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&serde_json::to_vec_pretty(&d.settings)?)?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    })();
    if let Err(error) = result {
        d.error = Some(error.to_string());
    }
}

fn load_preset(d: &mut Develop) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("RAW settings", &["json"])
        .pick_file()
    else {
        return;
    };
    let result = (|| -> anyhow::Result<DevelopSettings> {
        let mut bytes = Vec::new();
        File::open(path)?.take(1_048_577).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 1_048_576, "RAW settings file exceeds 1 MiB");
        let settings: DevelopSettings = serde_json::from_slice(&bytes)?;
        settings.validate()?;
        Ok(settings)
    })();
    match result {
        Ok(settings) => {
            d.settings = settings;
            d.selected_overlay = None;
        }
        Err(error) => d.error = Some(format!("Could not load RAW settings: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Pos2, Rect, vec2};

    /// The grading section the way a reader meets it: collapsed at the end of a
    /// page and opened by its header. The clock is handed in because egui reads
    /// two clicks in the same 300 ms as one double click, whatever they landed
    /// on.
    fn frame(
        ctx: &egui::Context,
        state: &mut Grading,
        time: f64,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(760.0, 420.0))),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    egui::CollapsingHeader::new("Color grading")
                        .default_open(true)
                        .show(ui, |ui| {
                            grading(ui, state);
                        });
                });
            },
        )
    }

    /// A press and a release `gap` seconds after the last click.
    fn tap(ctx: &egui::Context, state: &mut Grading, at: Pos2, now: &mut f64, gap: f64) {
        let events = |pressed| {
            vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        frame(ctx, state, *now, events(true));
        *now += gap;
        frame(ctx, state, *now, events(false));
    }

    fn text_pos(output: &egui::FullOutput, wanted: &str) -> Pos2 {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == wanted => Some(text.pos),
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing the {wanted} label"))
    }

    /// The first disc painted, found by its rim: no other control on the page
    /// draws a circle that large.
    fn first_disc(output: &egui::FullOutput) -> (Pos2, f32) {
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Circle(circle) if circle.radius >= 20.0 => {
                    Some((circle.center, circle.radius))
                }
                _ => None,
            })
            .min_by(|a, b| a.0.x.total_cmp(&b.0.x))
            .unwrap_or_else(|| panic!("Missing a grading disc"))
    }

    /// A reader opens the section, then picks a tint off a disc: the top of it
    /// is red at full strength and the right of it yellow, and only the wheel
    /// under the pointer moves.
    #[test]
    fn a_grading_disc_picks_the_tint_under_the_pointer() {
        let ctx = egui::Context::default();
        let mut state = Grading::default();
        let mut now = 0.0;
        let opened = frame(&ctx, &mut state, now, Vec::new());
        text_pos(&opened, "Color grading");
        let (center, radius) = first_disc(&opened);
        for (offset, hue) in [
            (vec2(0.0, -radius + 1.0), 0.0),
            (vec2(radius - 1.0, 0.0), 90.0),
            (vec2(0.0, radius - 1.0), 180.0),
        ] {
            tap(&ctx, &mut state, center + offset, &mut now, 0.5);
            frame(&ctx, &mut state, now, Vec::new());
            let wheel = state.shadows;
            assert!(
                (wheel.hue - hue).abs() < 1.0,
                "the pointer at {offset:?} picked hue {}",
                wheel.hue
            );
            assert!(
                wheel.saturation > 95.0,
                "at full strength: {}",
                wheel.saturation
            );
            assert!(!state.midtones.adjusts(), "the other wheels are untouched");
        }
    }

    /// A double click on a disc returns that wheel to neutral, the way a double
    /// click on a slider track returns it to its default.
    #[test]
    fn a_double_click_on_a_disc_returns_the_wheel_to_neutral() {
        let ctx = egui::Context::default();
        let mut state = Grading {
            shadows: GradeWheel {
                hue: 210.0,
                saturation: 60.0,
                luminance: -30.0,
            },
            blending: 20.0,
            ..Grading::default()
        };
        let mut now = 0.0;
        let (center, radius) = first_disc(&frame(&ctx, &mut state, now, Vec::new()));
        let at = center + vec2(0.0, -(radius - 4.0));
        tap(&ctx, &mut state, at, &mut now, 0.1);
        assert!(
            state.shadows.saturation > 90.0 && state.shadows.luminance == -30.0,
            "the tap moved the tint and left the shift alone: {:?}",
            state.shadows
        );
        tap(&ctx, &mut state, at, &mut now, 0.05);
        frame(&ctx, &mut state, now, Vec::new());
        assert_eq!(
            state.shadows,
            GradeWheel::default(),
            "the second click of the pair returns the wheel to neutral"
        );
        assert_eq!(state.blending, 20.0, "the sliders beside it are untouched");
    }
}
