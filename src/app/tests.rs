use super::*;

// Tests that publish images share the desktop's system clipboard.
static CLIPBOARD_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn psd_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn psd_i16(bytes: &mut Vec<u8>, value: i16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn psd_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn psd_i32(bytes: &mut Vec<u8>, value: i32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn minimal_psd_bytes() -> Vec<u8> {
    let mut extra = Vec::new();
    psd_u32(&mut extra, 0);
    psd_u32(&mut extra, 0);
    extra.push(1);
    extra.push(b'A');
    extra.extend([0; 2]);
    let mut record = Vec::new();
    for value in [0, 0, 2, 2] {
        psd_i32(&mut record, value);
    }
    psd_u16(&mut record, 4);
    for id in [0i16, 1, 2, -1] {
        psd_i16(&mut record, id);
        psd_u32(&mut record, 6);
    }
    record.extend_from_slice(b"8BIMnorm");
    record.extend([255, 0, 0, 0]);
    psd_u32(&mut record, extra.len() as u32);
    record.extend(extra);
    let mut layer_info = Vec::new();
    psd_i16(&mut layer_info, 1);
    layer_info.extend(record);
    for values in [
        [255u8, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 255, 255],
    ] {
        layer_info.extend([0, 0]);
        layer_info.extend(values);
    }
    let mut layer_mask = Vec::new();
    psd_u32(&mut layer_mask, layer_info.len() as u32);
    layer_mask.extend(layer_info);
    psd_u32(&mut layer_mask, 0);
    let mut result = Vec::new();
    result.extend_from_slice(b"8BPS");
    psd_u16(&mut result, 1);
    result.extend([0; 6]);
    psd_u16(&mut result, 4);
    psd_u32(&mut result, 2);
    psd_u32(&mut result, 2);
    psd_u16(&mut result, 8);
    psd_u16(&mut result, 3);
    psd_u32(&mut result, 0);
    psd_u32(&mut result, 0);
    psd_u32(&mut result, layer_mask.len() as u32);
    result.extend(layer_mask);
    result.extend([0; 18]);
    result
}

#[test]
fn opens_svg_as_layer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("vector.svg");
    std::fs::write(
        &path,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="3"><rect width="4" height="3" fill="#0000ff"/></svg>"##,
    )
    .unwrap();
    let (_context, mut app) = app();
    app.dimensions = [8, 6];
    app.new_document();
    app.open_path(&path, true);
    assert!(app.error.is_none(), "{:?}", app.error);
    let document = &app.session().unwrap().document;
    assert_eq!(document.layers.len(), 2);
    let layer = document.active().unwrap();
    assert_eq!(layer.transform.x, 2.0);
    assert_eq!(layer.transform.y, 1.5);
    assert_eq!(
        layer.pixels.as_ref().unwrap().get_pixel(0, 0).0,
        [0, 0, 255, 255]
    );
}

#[test]
fn opens_psd_as_a_new_project() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sample.psd");
    std::fs::write(&path, minimal_psd_bytes()).unwrap();
    let (_context, mut app) = app();
    app.open_path(&path, true);
    assert!(app.error.is_none(), "{:?}", app.error);
    assert_eq!(app.sessions.len(), 1);
    assert_eq!(app.session().unwrap().title, "sample");
    assert!(app.session().unwrap().path.is_none());
    assert_eq!(app.session().unwrap().document.layers.len(), 1);

    let invalid = directory.path().join("invalid.psd");
    std::fs::write(&invalid, b"not a psd").unwrap();
    app.open_path(&invalid, false);
    assert_eq!(app.sessions.len(), 1);
    assert!(app.error.is_some());
}

#[test]
fn text_tool_creates_edits_and_undoes_one_transaction() {
    let (context, mut app) = app();
    app.dimensions = [640, 480];
    app.new_document();
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::T, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert!(app.tool == Tool::Text);
    click_canvas(
        &context,
        &mut app,
        Point::new(40.0, 50.0),
        egui::Modifiers::NONE,
    );
    assert!(app.dialog == Some(Dialog::Text));
    assert_eq!(app.session().unwrap().document.layers.len(), 2);
    let id = app.session().unwrap().document.active.unwrap();
    assert!(
        (app.session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .transform
            .x
            - 40.0)
            .abs()
            < 0.01
    );
    let style = &mut app.text_edit.as_mut().unwrap().style;
    style.content = "Editable text\nSecond line".into();
    style.bold = true;
    style.italic = true;
    style.underline = true;
    style.strikethrough = true;
    app.preview_text();
    frame(&context, &mut app);
    assert_eq!(app.session().unwrap().history.names().count(), 0);
    app.finish_text(true);
    let session = app.session().unwrap();
    let pixels = session.document.active().unwrap().pixels.clone();
    assert_eq!(session.history.names().count(), 1);
    assert!(
        session
            .document
            .active()
            .unwrap()
            .text
            .as_ref()
            .unwrap()
            .bold
    );

    app.start_text(Some(id), Point::default());
    app.text_edit.as_mut().unwrap().style.content = "Revised".into();
    app.preview_text();
    app.finish_text(true);
    assert_eq!(app.session().unwrap().document.active.unwrap(), id);
    assert_eq!(app.session().unwrap().document.layers.len(), 2);
    app.command("undo");
    assert_eq!(
        app.session().unwrap().document.active().unwrap().pixels,
        pixels
    );
    app.command("undo");
    assert_eq!(app.session().unwrap().document.layers.len(), 1);
    app.command("redo");
    assert_eq!(
        app.session().unwrap().document.active().unwrap().pixels,
        pixels
    );
}

#[test]
fn coloured_letters_chosen_in_the_text_dialog_reach_the_layer() {
    let (context, mut app) = app();
    app.dimensions = [640, 480];
    app.new_document();
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::T, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    click_canvas(
        &context,
        &mut app,
        Point::new(40.0, 50.0),
        egui::Modifiers::NONE,
    );
    let style = &mut app.text_edit.as_mut().unwrap().style;
    style.content = "Red then blue".into();
    style.size = 64.0;
    // The dialog colours the letters the user selected, which the style holds as
    // a run over the first four characters.
    style.set_color_run(0, 4, [220, 20, 20, 255]);
    app.preview_text();
    frame(&context, &mut app);
    app.finish_text(true);
    let session = app.session().unwrap();
    let text = session.document.active().unwrap().text.as_ref().unwrap();
    assert_eq!(text.color_runs.len(), 1);
    assert_eq!(text.color_at(0), [220, 20, 20, 255]);
    assert_eq!(text.color_at(4), text.color);
    let pixels = session.document.active().unwrap().pixels.as_ref().unwrap();
    assert!(
        pixels
            .pixels()
            .any(|pixel| pixel[0] > 150 && pixel[1] < 100 && pixel[2] < 100),
        "the selected letters are painted"
    );
    // Clearing the colours goes back to one colour for the whole text.
    let id = session.document.active.unwrap();
    app.start_text(Some(id), Point::default());
    app.text_edit.as_mut().unwrap().style.clear_color_runs();
    app.preview_text();
    frame(&context, &mut app);
    app.finish_text(true);
    let text = app
        .session()
        .unwrap()
        .document
        .active()
        .unwrap()
        .text
        .as_ref()
        .unwrap();
    assert!(text.color_runs.is_empty());
}

#[test]
fn the_camera_raw_filter_panel_pages_through_and_keeps_its_settings() {
    let (context, mut app) = app();
    app.dimensions = [320, 240];
    app.new_document();
    frame(&context, &mut app);
    app.start_filter(mectov::effects::Filter::CameraRaw {
        settings: Box::new(mectov::raw::DevelopSettings {
            sharpen: 0.0,
            color_noise: 0.0,
            luminance_noise: 0.0,
            ..Default::default()
        }),
        temperature: 0.0,
        tint: 0.0,
    });
    assert!(app.dialog == Some(Dialog::Effect));
    // The Tone page is longer than the panel is tall, so the panel is opened on
    // a taller screen: a window keeps the place it was given, and its body
    // scrolls inside a height taken from the screen it was opened on.
    tall_frame(&context, &mut app);
    // Every page draws, and the page the user is on is kept between frames.
    for page in 0..8 {
        let mut edit = app.effect.take().unwrap();
        edit.camera_raw_page = page;
        edit.refresh = true;
        app.effect = Some(edit);
        tall_frame(&context, &mut app);
        assert!(app.error.is_none(), "page {page}: {:?}", app.error);
        assert_eq!(app.effect.as_ref().unwrap().camera_raw_page, page);
    }
    // The Effects page carries the presence controls, glow, the post-crop
    // vignette and grain, and the Calibration page the process picker and the
    // three primaries, the way Compositor's own sections do.
    for (page, wanted) in [
        (
            3,
            [
                "Clarity",
                "Glow",
                "Style",
                "Amount",
                "Midpoint",
                "Size",
                "Roughness",
            ],
        ),
        (
            6,
            [
                "Version 6",
                "Shadows",
                "Tint",
                "Red primary",
                "Hue",
                "Saturation",
                "Blue primary",
            ],
        ),
    ] {
        let mut edit = app.effect.take().unwrap();
        edit.camera_raw_page = page;
        app.effect = Some(edit);
        let output = tall_frame(&context, &mut app);
        let texts: Vec<&str> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect();
        for label in wanted {
            assert!(
                texts.contains(&label),
                "page {page} offers {label}: {texts:?}"
            );
        }
    }

    // The Tone page carries the grading wheels, at the end of the page beside
    // the split toning they follow.
    let mut edit = app.effect.take().unwrap();
    edit.camera_raw_page = 1;
    app.effect = Some(edit);
    let output = tall_frame(&context, &mut app);
    let texts: Vec<&str> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some(text.galley.text()),
            _ => None,
        })
        .collect();
    assert!(
        texts.contains(&"Color grading"),
        "the filter panel offers the wheels: {texts:?}"
    );
    // The section is open, as Compositor 1.3.2 has it, and picking a tint off a
    // disc edits the filter's own settings: the same wheels the Develop panel
    // shows, reached through the panel's own controls.
    let opened = tall_frame(&context, &mut app);
    let (center, radius) = opened
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Circle(circle) if circle.radius >= 20.0 => {
                Some((circle.center, circle.radius))
            }
            _ => None,
        })
        .min_by(|a, b| a.0.x.total_cmp(&b.0.x))
        .expect("the opened section draws a wheel");
    let top = center + Vec2::new(0.0, -radius + 1.0);
    tall_pointer(&context, &mut app, top, Some(true), 3.0);
    tall_pointer(&context, &mut app, top, Some(false), 3.0);
    tall_frame(&context, &mut app);
    let edit = app.effect.as_ref().unwrap();
    let Filter::CameraRaw { settings, .. } = edit.filter.as_ref().unwrap() else {
        panic!("the filter is still the Camera Raw filter");
    };
    assert!(
        settings.grading.shadows.saturation > 95.0 && settings.grading.shadows.hue < 1.0,
        "the wheel under the pointer reached the filter: {:?}",
        settings.grading.shadows
    );

    // The controls reach the filter, and the filter keeps them.
    let mut edit = app.effect.take().unwrap();
    edit.camera_raw_page = 0;
    if let mectov::effects::Filter::CameraRaw { settings, .. } = edit.filter.as_mut().unwrap() {
        settings.exposure = 1.25;
        settings.monochrome = true;
    }
    app.effect = Some(edit);
    frame(&context, &mut app);
    let edit = app.effect.as_ref().unwrap();
    if let mectov::effects::Filter::CameraRaw { settings, .. } = edit.filter.as_ref().unwrap() {
        assert_eq!(settings.exposure, 1.25);
        assert!(settings.monochrome);
    } else {
        panic!("the filter is still the Camera Raw filter");
    }
    // Cancelling leaves the document as it was.
    let before = app.session().unwrap().document.layers[0].pixels.clone();
    let event = egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    keyboard_frame(&context, &mut app, vec![event], egui::Modifiers::NONE);
    assert!(app.effect.is_none());
    assert!(app.dialog.is_none());
    assert_eq!(app.session().unwrap().document.layers[0].pixels, before);
}

#[test]
fn a_double_click_on_a_filter_slider_returns_it_to_its_neutral_value() {
    let (context, mut app) = app();
    app.dimensions = [320, 240];
    app.new_document();
    frame(&context, &mut app);
    app.start_filter(Filter::TonalContrast {
        amount: 50.0,
        radius: 42.0,
        shadows: 40.0,
        midtones: 60.0,
        highlights: 30.0,
    });
    for _ in 0..2 {
        frame(&context, &mut app);
    }
    let radius = track_right_of(&context, &mut app, "Radius");
    pointer_frame(
        &context,
        &mut app,
        radius,
        Some(true),
        egui::Modifiers::NONE,
    );
    pointer_frame(
        &context,
        &mut app,
        radius,
        Some(false),
        egui::Modifiers::NONE,
    );
    pointer_frame(
        &context,
        &mut app,
        radius,
        Some(true),
        egui::Modifiers::NONE,
    );
    frame(&context, &mut app);
    pointer_frame(
        &context,
        &mut app,
        radius,
        Some(false),
        egui::Modifiers::NONE,
    );
    frame(&context, &mut app);
    let edit = app.effect.as_ref().expect("the dialog is still open");
    match edit.filter.as_ref().expect("the filter is still open") {
        Filter::TonalContrast { radius, .. } => {
            assert_eq!(*radius, 16.0, "the radius went back to its own default")
        }
        other => panic!("still the {other:?} filter"),
    }
    assert!(app.error.is_none(), "{:?}", app.error);
}

fn text_key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
fn font_picker_arrows_preview_filtered_fonts_without_editing_text() {
    let (context, mut app) = app();
    app.dimensions = [640, 480];
    app.new_document();
    app.start_text(None, Point::new(25.0, 35.0));
    frame(&context, &mut app);
    let original = app.text_edit.as_ref().unwrap().style.clone();
    let families = app.text_renderer.as_ref().unwrap().families().to_vec();
    let start = families
        .iter()
        .position(|family| family == &original.family)
        .unwrap();
    let position = layer_label(&context, &mut app, &original.family) + egui::vec2(5.0, 5.0);
    pointer_frame(
        &context,
        &mut app,
        position,
        Some(true),
        egui::Modifiers::NONE,
    );
    pointer_frame(
        &context,
        &mut app,
        position,
        Some(false),
        egui::Modifiers::NONE,
    );
    frame(&context, &mut app);
    assert!(egui::Popup::is_any_open(&context));

    for step in 1..=12 {
        keyboard_frame(
            &context,
            &mut app,
            vec![text_key(egui::Key::ArrowDown, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        let style = &app.text_edit.as_ref().unwrap().style;
        assert_eq!(
            style.family,
            families[(start + step).min(families.len() - 1)]
        );
        assert_eq!(style.content, original.content);
        let layer = app.session().unwrap().document.active().unwrap();
        assert_eq!(layer.text.as_ref().unwrap(), style);
        assert!(egui::Popup::is_any_open(&context));
        assert_eq!(app.session().unwrap().history.names().count(), 0);
    }
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::ArrowUp, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert_eq!(
        app.text_edit.as_ref().unwrap().style.family,
        families[(start + 12).min(families.len() - 1).saturating_sub(1)]
    );
    let style = app.text_edit.as_ref().unwrap().style.clone();
    let expected = app.text_renderer.as_mut().unwrap().render(&style).unwrap();
    assert_eq!(
        app.session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .pixels
            .as_deref(),
        Some(&expected)
    );

    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Text(original.family.clone())],
        egui::Modifiers::NONE,
    );
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::ArrowDown, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert_eq!(
        app.text_edit.as_ref().unwrap().style.family,
        original.family
    );
    assert_eq!(
        app.text_edit.as_ref().unwrap().style.content,
        original.content
    );
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Text(" no matching font".into())],
        egui::Modifiers::NONE,
    );
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::ArrowDown, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert_eq!(
        app.text_edit.as_ref().unwrap().style.family,
        original.family
    );
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::Escape, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert!(!egui::Popup::is_any_open(&context));
    assert!(app.dialog == Some(Dialog::Text));
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::Escape, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert!(app.dialog.is_none());
    assert_eq!(app.session().unwrap().document.layers.len(), 1);
}

#[test]
fn text_dialog_typing_apply_and_escape_do_not_trigger_canvas_shortcuts() {
    let (context, mut app) = app();
    app.dimensions = [640, 480];
    app.new_document();
    app.start_text(None, Point::new(25.0, 35.0));
    frame(&context, &mut app);
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Text("Typing B and T".into())],
        egui::Modifiers::NONE,
    );
    assert_eq!(
        app.text_edit.as_ref().unwrap().style.content,
        "Typing B and T"
    );
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::Enter, egui::Modifiers::CTRL)],
        egui::Modifiers::CTRL,
    );
    assert!(app.dialog.is_none());
    let session = app.session().unwrap();
    assert_eq!(
        session
            .document
            .active()
            .unwrap()
            .text
            .as_ref()
            .unwrap()
            .content,
        "Typing B and T"
    );
    let original = session.document.active().unwrap().pixels.clone();
    let id = session.document.active.unwrap();
    app.start_text(Some(id), Point::default());
    frame(&context, &mut app);
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Paste("Changed".into())],
        egui::Modifiers::CTRL,
    );
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::Escape, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert!(app.dialog.is_none());
    assert_eq!(
        app.session().unwrap().document.active().unwrap().pixels,
        original
    );
    assert_eq!(app.session().unwrap().history.names().count(), 1);

    app.start_text(None, Point::new(20.0, 20.0));
    frame(&context, &mut app);
    app.finish_text(false);
    assert_eq!(app.session().unwrap().document.layers.len(), 2);
    app.session_mut()
        .unwrap()
        .document
        .active_mut()
        .unwrap()
        .locked = true;
    app.start_text(Some(id), Point::default());
    assert!(app.dialog.is_none());
}

#[test]
fn text_preview_keeps_group_parent_and_rejects_invalid_edits() {
    let (_, mut app) = app();
    app.dimensions = [640, 480];
    app.new_document();
    app.command("group");
    let group = app.session().unwrap().document.active.unwrap();
    app.start_text(None, Point::new(10.0, 20.0));
    app.text_edit.as_mut().unwrap().style.content = "In a folder".into();
    app.preview_text();
    assert_eq!(
        app.session().unwrap().document.active().unwrap().parent,
        Some(group)
    );
    let pixels = app
        .session()
        .unwrap()
        .document
        .active()
        .unwrap()
        .pixels
        .clone();
    app.text_edit.as_mut().unwrap().style.size = f32::NAN;
    app.preview_text();
    app.finish_text(true);
    assert!(app.dialog == Some(Dialog::Text));
    assert_eq!(
        app.session().unwrap().document.active().unwrap().pixels,
        pixels
    );
    app.text_edit.as_mut().unwrap().style.size = 24.0;
    app.preview_text();
    app.finish_text(true);
    assert!(app.dialog.is_none());
    app.session().unwrap().document.validate().unwrap();
}

#[test]
#[ignore = "requires a GPU; optionally set MECTOV_ZOOM_BENCH_IMAGE to an image path"]
fn benchmark_large_image_zoom() {
    let (context, mut app, state) = large_image_benchmark_app();
    app.tool = Tool::Hand;
    for _ in 0..3 {
        frame(&context, &mut app);
    }
    state
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let mut durations = Vec::new();
    for step in (0..12).chain((0..12).rev()).cycle().take(48) {
        app.session_mut().unwrap().zoom = 0.8_f32.powi(step);
        let start = std::time::Instant::now();
        let output = frame(&context, &mut app);
        let _ = context.tessellate(output.shapes, output.pixels_per_point);
        state
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        durations.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    report_benchmark("Zoom UI + compositor", durations);
}

#[test]
#[ignore = "requires a GPU; optionally set MECTOV_ZOOM_BENCH_IMAGE to an image path"]
fn benchmark_large_image_levels() {
    let (context, mut app, state) = large_image_benchmark_app();
    app.start_adjustment(
        Adjustment::LevelsChannels {
            ranges: [mectov::color::DEFAULT_LEVELS; 4],
        },
        true,
    );
    for _ in 0..3 {
        frame(&context, &mut app);
    }
    for refresh in [false, true] {
        let mut durations = Vec::new();
        for step in 0..24 {
            let edit = app.effect.as_mut().unwrap();
            if refresh {
                let Some(Adjustment::LevelsChannels { ranges }) = &mut edit.adjustment else {
                    unreachable!();
                };
                ranges[0][0] = step as f32;
                edit.refresh = true;
            }
            let start = std::time::Instant::now();
            let output = pointer_frame(
                &context,
                &mut app,
                Pos2::new(500.0 + step as f32, 250.0),
                None,
                egui::Modifiers::NONE,
            );
            let _ = context.tessellate(output.shapes, output.pixels_per_point);
            state
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            durations.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        report_benchmark(
            if refresh {
                "Levels live preview"
            } else {
                "Levels pointer movement"
            },
            durations,
        );
    }
    assert!(app.error.is_none(), "{:?}", app.error);
}

#[test]
#[ignore = "requires a GPU; optionally set MECTOV_ZOOM_BENCH_IMAGE to an image path"]
fn benchmark_large_image_motion_blur() {
    let (context, mut app, state) = large_image_benchmark_app();
    eprintln!("Motion Blur adapter: {:?}", state.adapter.get_info());
    for _ in 0..3 {
        frame(&context, &mut app);
    }
    app.start_filter(Filter::MotionBlur {
        distance: 15.0,
        angle: 0.0,
    });
    for _ in 0..3 {
        frame(&context, &mut app);
    }
    state
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    for distance in [15.0, 200.0] {
        let mut durations = Vec::new();
        for step in 0..24 {
            let edit = app.effect.as_mut().unwrap();
            edit.filter = Some(Filter::MotionBlur {
                distance,
                angle: step as f32 * 3.0,
            });
            edit.refresh = true;
            let start = std::time::Instant::now();
            // The dialog updates settings after drawing the canvas, so include
            // the following frame and GPU completion in end-to-end latency.
            frame(&context, &mut app);
            frame(&context, &mut app);
            state
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            durations.push(start.elapsed().as_secs_f64() * 1000.0);
            assert!(!app.effect.as_ref().unwrap().filter_preview.busy());
            assert!(app.session().unwrap().motion_blur_preview.is_some());
        }
        report_benchmark(&format!("Motion Blur distance {distance}"), durations);
    }
    assert!(app.error.is_none(), "{:?}", app.error);
}

#[test]
#[ignore = "requires a GPU; optionally set MECTOV_ZOOM_BENCH_IMAGE to an image path"]
fn benchmark_large_image_motion_blur_apply() {
    let (context, mut app, state) = large_image_benchmark_app();
    frame(&context, &mut app);
    state
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let session = app.session().unwrap();
    let original = &session.document;
    let pixels = original.active().unwrap().pixels.as_ref().unwrap();
    let worker = session.gpu.as_ref().unwrap().motion_blur_worker(pixels);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    eprintln!(
        "GPU Apply adapter: {:?}, source {}x{}",
        state.adapter.get_info(),
        pixels.width(),
        pixels.height()
    );
    for distance in [15.0, 200.0] {
        let filter = Filter::MotionBlur {
            distance,
            angle: 35.0,
        };
        let padding = (distance * 0.5_f32).ceil() as u32 + 1;
        let start = std::time::Instant::now();
        let result = worker
            .render(pixels, distance, 35.0, padding, &cancel)
            .unwrap()
            .expect("GPU must execute the benchmark");
        let gpu_time = start.elapsed();
        let mut expected = original.clone();
        let start = std::time::Instant::now();
        mectov::effects::apply_filter(&mut expected, &filter, false).unwrap();
        let cpu_time = start.elapsed();
        let expected = expected.active().unwrap().pixels.as_ref().unwrap();
        assert_eq!(result.dimensions(), expected.dimensions());
        assert!(
            result
                .as_raw()
                .iter()
                .zip(expected.as_raw())
                .all(|(a, b)| a.abs_diff(*b) <= 1)
        );
        eprintln!(
            "Full-resolution Apply distance {distance}: GPU + readback {gpu_time:?}, CPU {cpu_time:?}, {:.1}x faster",
            cpu_time.as_secs_f64() / gpu_time.as_secs_f64()
        );
    }
}

#[test]
#[ignore = "requires a Vulkan or OpenGL compute adapter; run explicitly for native verification"]
fn motion_blur_preview_toggles_cancels_and_applies_full_resolution() {
    let (context, mut app, state) = large_image_benchmark_app();
    let adapter = state.adapter.get_info();
    eprintln!("Motion Blur adapter: {adapter:?}");
    // Software adapters intentionally use the asynchronous CPU preview.
    let gpu_preview = adapter.device_type != wgpu::DeviceType::Cpu;
    app.sessions = vec![Session::new(
        Document::new(32, 24).unwrap(),
        "Blur test".into(),
        None,
    )];
    app.brush.color = [210, 80, 40, 255];
    app.command("fill_fg");
    frame(&context, &mut app);
    assert_eq!(app.session().unwrap().gpu.is_some(), gpu_preview);
    let original = app.session().unwrap().document.clone();
    let pixels = original.active().unwrap().pixels.as_ref().unwrap();
    let revision = app.session().unwrap().history.revision;
    let filter = Filter::MotionBlur {
        distance: 20.0,
        angle: 35.0,
    };
    let mut expected = original.clone();
    mectov::effects::apply_filter(&mut expected, &filter, false).unwrap();
    app.start_filter(filter.clone());
    for preview in [true, false, true] {
        let edit = app.effect.as_mut().unwrap();
        edit.preview = preview;
        edit.refresh = true;
        frame(&context, &mut app);
        frame(&context, &mut app);
        state
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        if !gpu_preview {
            wait_for_filter_preview(&context, &mut app);
        }
        assert_eq!(
            app.session().unwrap().motion_blur_preview.is_some(),
            preview && gpu_preview
        );
        assert!(!app.effect.as_ref().unwrap().filter_preview.busy());
        assert_eq!(app.session().unwrap().history.revision, revision);
        let preview_pixels = app
            .session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .pixels
            .as_ref()
            .unwrap();
        if preview && !gpu_preview {
            assert_eq!(
                preview_pixels,
                expected.active().unwrap().pixels.as_ref().unwrap()
            );
        } else {
            assert!(Arc::ptr_eq(pixels, preview_pixels));
        }
    }

    let apply = layer_label(&context, &mut app, "Apply") + Vec2::splat(5.0);
    pointer_frame(&context, &mut app, apply, None, egui::Modifiers::NONE);
    pointer_frame(&context, &mut app, apply, Some(true), egui::Modifiers::NONE);
    pointer_frame(
        &context,
        &mut app,
        apply,
        Some(false),
        egui::Modifiers::NONE,
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.effect.is_some() {
        frame(&context, &mut app);
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert!(app.dialog.is_none());
    assert!(app.session().unwrap().motion_blur_preview.is_none());
    assert_eq!(app.session().unwrap().history.revision, revision + 1);
    assert_eq!(
        app.session().unwrap().document.active().unwrap().pixels,
        expected.active().unwrap().pixels
    );
    app.command("undo");
    assert_eq!(
        app.session().unwrap().document.active().unwrap().pixels,
        original.active().unwrap().pixels
    );
    app.command("redo");
    assert_eq!(
        app.session().unwrap().document.active().unwrap().pixels,
        expected.active().unwrap().pixels
    );

    app.start_filter(filter);
    frame(&context, &mut app);
    frame(&context, &mut app);
    if !gpu_preview {
        wait_for_filter_preview(&context, &mut app);
    }
    assert_eq!(
        app.session().unwrap().motion_blur_preview.is_some(),
        gpu_preview
    );
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::Escape, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert!(app.session().unwrap().motion_blur_preview.is_none());
    assert!(app.dialog.is_none());
    assert_eq!(app.session().unwrap().history.revision, revision + 1);
    assert_eq!(
        app.session().unwrap().document.active().unwrap().pixels,
        expected.active().unwrap().pixels
    );
    assert!(app.error.is_none(), "{:?}", app.error);
}

fn wait_for_filter_preview(context: &egui::Context, app: &mut EditorApp) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.effect.as_ref().unwrap().filter_preview.busy() {
        frame(context, app);
        assert!(
            std::time::Instant::now() < deadline,
            "Filter preview worker did not finish"
        );
        std::thread::yield_now();
    }
}

fn large_image_benchmark_app() -> (egui::Context, EditorApp, eframe::egui_wgpu::RenderState) {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .unwrap();
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    let target_format = wgpu::TextureFormat::Rgba8Unorm;
    let renderer = eframe::egui_wgpu::Renderer::new(&device, target_format, Default::default());
    let state = eframe::egui_wgpu::RenderState {
        adapter,
        available_adapters: Vec::new(),
        device,
        queue,
        target_format,
        renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
    };
    let pixels = std::env::var_os("MECTOV_ZOOM_BENCH_IMAGE").map_or_else(
        || {
            RgbaImage::from_fn(3000, 3000, |x, y| {
                image::Rgba([x as u8, y as u8, (x + y) as u8, 255])
            })
        },
        |path| io::import_image(Path::new(&path)).unwrap(),
    );
    let mut document = Document::new(pixels.width(), pixels.height()).unwrap();
    document.layers = vec![Layer::image("Large image", pixels)];
    document.select(document.layers[0].id, false);
    let (context, mut app) = app();
    app.gpu_state = Some(state.clone());
    app.sessions
        .push(Session::new(document, "Zoom benchmark".into(), None));
    (context, app, state)
}

fn report_benchmark(name: &str, mut durations: Vec<f64>) {
    durations.sort_by(f64::total_cmp);
    let count = durations.len();
    eprintln!(
        "{name} ({count} frames): median {:.2} ms, p95 {:.2} ms, max {:.2} ms",
        durations[count / 2],
        durations[count * 95 / 100],
        durations[count - 1],
    );
}

#[test]
#[ignore = "requires a GPU; optionally set MECTOV_ZOOM_BENCH_IMAGE to an image path"]
fn benchmark_large_image_editing() {
    let (context, mut app, state) = large_image_benchmark_app();
    let document = app.session().unwrap().document.clone();
    for tool in [
        Tool::Move,
        Tool::Marquee,
        Tool::Lasso,
        Tool::Brush,
        Tool::Erase,
    ] {
        app.sessions = vec![Session::new(
            document.clone(),
            "Editing benchmark".into(),
            None,
        )];
        app.tool = tool;
        app.snap = false;
        for _ in 0..3 {
            frame(&context, &mut app);
        }
        let rect = app.canvas_rect.unwrap();
        let start = rect.min + rect.size() * Vec2::new(0.25, 0.4);
        pointer_frame(&context, &mut app, start, Some(true), egui::Modifiers::NONE);
        let mut durations = Vec::new();
        let mut position = start;
        for step in 1..=24 {
            let progress = step as f32 / 24.0;
            position = start
                + rect.size()
                    * Vec2::new(
                        progress * 0.4,
                        (progress * std::f32::consts::TAU).sin() * 0.15,
                    );
            let now = std::time::Instant::now();
            let output = pointer_frame(&context, &mut app, position, None, egui::Modifiers::NONE);
            let _ = context.tessellate(output.shapes, output.pixels_per_point);
            state
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            durations.push(now.elapsed().as_secs_f64() * 1000.0);
        }
        report_benchmark(tool.label(), durations);
        let now = std::time::Instant::now();
        pointer_frame(
            &context,
            &mut app,
            position,
            Some(false),
            egui::Modifiers::NONE,
        );
        for _ in 0..2 {
            frame(&context, &mut app);
        }
        state
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        eprintln!(
            "{} release + settle: {:.2} ms",
            tool.label(),
            now.elapsed().as_secs_f64() * 1000.0
        );
        assert!(app.error.is_none(), "{:?}", app.error);
    }
}

#[test]
fn zoom_reuses_preview_and_thumbnails_until_document_changes() {
    let (context, mut app) = app();
    app.dimensions = [64, 48];
    app.new_document();
    for _ in 0..3 {
        frame(&context, &mut app);
    }
    let session = app.session().unwrap();
    let composite = session.composite.clone().unwrap();
    let texture = session.texture.as_ref().unwrap().id();
    let thumbnails: HashMap<_, _> = session
        .thumbnails
        .iter()
        .map(|(key, texture)| (*key, texture.id()))
        .collect();
    assert!(!thumbnails.is_empty());

    for zoom in [1.0, 0.51, 0.25, 0.05, 8.0, 0.01, 64.0] {
        app.session_mut().unwrap().zoom = zoom;
        let output = frame(&context, &mut app);
        let session = app.session().unwrap();
        assert_eq!(session.preview_size, [64, 48]);
        assert!(Arc::ptr_eq(session.composite.as_ref().unwrap(), &composite));
        assert!(
            !output
                .textures_delta
                .set
                .iter()
                .any(|(id, _)| *id == texture)
        );
        assert_eq!(session.thumbnails.len(), thumbnails.len());
        for (key, id) in &thumbnails {
            assert_eq!(session.thumbnails[key].id(), *id);
        }
    }

    app.command("fill_fg");
    frame(&context, &mut app);
    frame(&context, &mut app);
    let session = app.session().unwrap();
    assert!(!Arc::ptr_eq(
        session.composite.as_ref().unwrap(),
        &composite
    ));
    for (key, id) in thumbnails {
        assert_ne!(session.thumbnails[&key].id(), id);
    }
}

#[test]
fn selection_gestures_and_commands_reuse_the_composition_and_remain_undoable() {
    for tool in [Tool::Marquee, Tool::Lasso] {
        let (context, mut app) = app();
        app.dimensions = [64, 48];
        app.new_document();
        app.command("fill_fg");
        app.tool = tool;
        for _ in 0..3 {
            frame(&context, &mut app);
        }
        let session = app.session().unwrap();
        let composite = session.composite.clone().unwrap();
        let thumbnails: HashMap<_, _> = session
            .thumbnails
            .iter()
            .map(|(key, texture)| (*key, texture.id()))
            .collect();
        let origin = app.canvas_rect.unwrap().min;
        let zoom = session.zoom;
        let positions = [(10.0, 10.0), (40.0, 12.0), (45.0, 35.0)]
            .map(|(x, y)| origin + Vec2::new(x, y) * zoom);
        pointer_frame(
            &context,
            &mut app,
            positions[0],
            Some(true),
            egui::Modifiers::NONE,
        );
        for position in &positions[1..] {
            pointer_frame(&context, &mut app, *position, None, egui::Modifiers::NONE);
            frame(&context, &mut app);
            assert!(!app.session().unwrap().dirty_preview);
        }
        pointer_frame(
            &context,
            &mut app,
            positions[2],
            Some(false),
            egui::Modifiers::NONE,
        );
        frame(&context, &mut app);
        let session = app.session().unwrap();
        assert!(session.document.selection.is_some());
        assert!(Arc::ptr_eq(session.composite.as_ref().unwrap(), &composite));
        for (key, id) in &thumbnails {
            assert_eq!(session.thumbnails[key].id(), *id);
        }
        app.command("undo");
        assert!(app.session().unwrap().document.selection.is_none());
        app.command("redo");
        assert!(app.session().unwrap().document.selection.is_some());
        frame(&context, &mut app);
        let composite = app.session().unwrap().composite.clone().unwrap();
        for command in ["select_all", "invert_selection", "deselect"] {
            app.command(command);
            frame(&context, &mut app);
            assert!(Arc::ptr_eq(
                app.session().unwrap().composite.as_ref().unwrap(),
                &composite
            ));
        }
    }
}

#[test]
fn painting_refreshes_only_the_changed_layer_thumbnail() {
    let (context, mut app) = app();
    app.dimensions = [64, 48];
    app.new_document();
    app.command("fill_fg");
    app.command("duplicate");
    for _ in 0..3 {
        frame(&context, &mut app);
    }
    let session = app.session().unwrap();
    let base = session.document.layers[0].id;
    let painted = session.document.active.unwrap();
    let original_base = session.thumbnails[&(base, false)].id();
    let original_painted = session.thumbnails[&(painted, false)].id();
    app.tool = Tool::Brush;
    app.brush.color = [255, 0, 0, 255];
    app.brush.diameter = 4.0;
    drag(
        &context,
        &mut app,
        Point::new(12.0, 12.0),
        Point::new(30.0, 20.0),
        egui::Modifiers::NONE,
    );
    frame(&context, &mut app);
    let session = app.session().unwrap();
    assert_eq!(session.thumbnails[&(base, false)].id(), original_base);
    assert_ne!(session.thumbnails[&(painted, false)].id(), original_painted);
    assert_eq!(
        session.document.layers[0]
            .pixels
            .as_ref()
            .unwrap()
            .get_pixel(20, 16)
            .0,
        [0, 0, 0, 255]
    );
    assert_eq!(
        session
            .document
            .active()
            .unwrap()
            .pixels
            .as_ref()
            .unwrap()
            .get_pixel(20, 16)
            .0,
        [255, 0, 0, 255]
    );
    app.command("undo");
    frame(&context, &mut app);
    assert_eq!(
        app.session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .pixels
            .as_ref()
            .unwrap()
            .get_pixel(20, 16)
            .0,
        [0, 0, 0, 255]
    );
}

fn app() -> (egui::Context, EditorApp) {
    let context = egui::Context::default();
    let app = EditorApp::with_context(&context, Vec::new(), false, None);
    (context, app)
}

fn keyboard_frame(
    context: &egui::Context,
    app: &mut EditorApp,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
) -> egui::FullOutput {
    context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(1280.0, 860.0),
            )),
            events,
            modifiers,
            ..Default::default()
        },
        |ctx| app.show(ctx),
    )
}

#[test]
fn layer_effect_dialog_previews_and_commits_one_undoable_edit() {
    let (context, mut app) = app();
    app.dimensions = [32, 24];
    app.new_document();
    app.command("fill_fg");
    let id = app.session().unwrap().document.active.unwrap();
    app.edit_layer_effects(id);
    assert!(app.dialog == Some(Dialog::Effect));
    frame(&context, &mut app);
    let edit = app.effect.as_mut().unwrap();
    edit.layer_effects.as_mut().unwrap().stroke = Some(mectov::document::StrokeEffect {
        size: 2.0,
        ..mectov::document::StrokeEffect::default()
    });
    edit.refresh = true;
    frame(&context, &mut app);
    let rendered = render::render(&app.session().unwrap().document);
    assert!(rendered.get_pixel(1, 12)[3] > 0);
    let apply = layer_label(&context, &mut app, "Apply") + Vec2::splat(5.0);
    pointer_frame(&context, &mut app, apply, None, egui::Modifiers::NONE);
    pointer_frame(&context, &mut app, apply, Some(true), egui::Modifiers::NONE);
    pointer_frame(
        &context,
        &mut app,
        apply,
        Some(false),
        egui::Modifiers::NONE,
    );
    assert!(app.dialog.is_none());
    assert!(app.effect.is_none());
    assert_eq!(
        app.session().unwrap().history.undo_name(),
        Some("Layer Effects")
    );
    app.command("undo");
    assert!(
        app.session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .effects
            .is_none()
    );
}

#[test]
fn whole_layer_clipboard_preserves_effects_and_is_undoable() {
    let (context, mut app) = app();
    let mut document = Document::new(24, 18).unwrap();
    document.layers.clear();
    let mut layer = Layer::image(
        "Structured",
        RgbaImage::from_pixel(6, 5, image::Rgba([30, 120, 240, 255])),
    );
    layer.transform.x = 7.0;
    layer.transform.y = 4.0;
    layer.effects = Some(mectov::document::LayerEffects {
        outer_glow: Some(mectov::document::OuterGlowEffect::default()),
        ..Default::default()
    });
    document.insert(layer);
    let id = document.active.unwrap();
    app.sessions
        .push(Session::new(document, "Layers".into(), None));
    assert!(app.copy_selected_layers(false));
    app.paste_content(super::clipboard::ClipboardContent::Layers);
    frame(&context, &mut app);
    let session = app.session().unwrap();
    assert_eq!(session.document.layers.len(), 2);
    let pasted = session
        .document
        .layers
        .iter()
        .find(|layer| layer.id != id)
        .unwrap();
    assert_eq!(
        pasted.effects.as_ref().unwrap().outer_glow,
        Some(mectov::document::OuterGlowEffect::default())
    );
    assert_eq!(pasted.transform.x, 7.0);
    app.command("undo");
    assert_eq!(app.session().unwrap().document.layers.len(), 1);
}

#[test]
fn native_clipboard_shortcuts_copy_cut_and_paste_selected_pixels() {
    let _clipboard_guard = CLIPBOARD_TEST_LOCK.lock().unwrap();
    let (context, mut app) = app();
    let mut document = Document::new(8, 6).unwrap();
    document.layers.clear();
    let source = RgbaImage::from_fn(8, 6, |x, _| {
        image::Rgba(if x < 4 { [255, 0, 0, 255] } else { [0; 4] })
    });
    document.insert(Layer::image(
        "Background",
        RgbaImage::from_pixel(8, 6, image::Rgba([0, 0, 255, 255])),
    ));
    document.insert(Layer::image("Source", source.clone()));
    let source_id = document.active.unwrap();
    app.sessions
        .push(Session::new(document, "Clipboard".into(), None));
    app.set_tool(Tool::Marquee);
    drag(
        &context,
        &mut app,
        Point::new(2.0, 1.0),
        Point::new(6.0, 4.0),
        egui::Modifiers::NONE,
    );
    assert_eq!(
        mectov::selection::bounds(
            app.session()
                .unwrap()
                .document
                .selection
                .as_deref()
                .unwrap()
        ),
        Some((2, 1, 6, 4))
    );
    let ctrl = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };

    // egui-winit emits clipboard events instead of C/X/V key presses.
    keyboard_frame(&context, &mut app, vec![egui::Event::Copy], ctrl);
    let (pixels, point) = app.clipboard.as_ref().expect("copy shortcut must run");
    assert_eq!(pixels.dimensions(), (4, 3));
    assert_eq!(*point, Point::new(2.0, 1.0));
    assert_eq!(pixels.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(pixels.get_pixel(3, 0).0, [0; 4]);

    // An image-only system clipboard has no text payload.
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Paste(String::new())],
        ctrl,
    );
    let document = &app.session().unwrap().document;
    assert_eq!(document.layers.len(), 3);
    let pasted = document.active().unwrap();
    assert_eq!(pasted.name, "Pasted image");
    assert_eq!(pasted.pixels.as_deref().unwrap().dimensions(), (4, 3));
    assert_eq!((pasted.transform.x, pasted.transform.y), (2.0, 1.0));
    assert_eq!(
        document
            .layers
            .iter()
            .find(|layer| layer.id == source_id)
            .unwrap()
            .pixels
            .as_deref()
            .unwrap(),
        &source
    );
    app.command("undo");
    assert_eq!(app.session().unwrap().document.layers.len(), 2);

    app.session_mut().unwrap().document.select(source_id, false);
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Copy],
        ctrl | egui::Modifiers::SHIFT,
    );
    assert_eq!(
        app.clipboard.as_ref().unwrap().0.get_pixel(3, 0).0,
        [0, 0, 255, 255]
    );

    keyboard_frame(&context, &mut app, vec![egui::Event::Cut], ctrl);
    let document = &app.session().unwrap().document;
    let pixels = document
        .layers
        .iter()
        .find(|layer| layer.id == source_id)
        .unwrap()
        .pixels
        .as_deref()
        .unwrap();
    assert_eq!(pixels.get_pixel(2, 1).0[3], 0);
    assert_eq!(pixels.get_pixel(0, 0).0, [255, 0, 0, 255]);
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Paste(String::new())],
        ctrl,
    );
    assert_eq!(app.session().unwrap().document.layers.len(), 3);
    assert_eq!(
        app.session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .pixels
            .as_deref()
            .unwrap()
            .get_pixel(0, 0)
            .0,
        [255, 0, 0, 255]
    );
}

#[test]
fn marquee_copy_without_an_active_layer_replaces_the_previous_clipboard() {
    let _clipboard_guard = CLIPBOARD_TEST_LOCK.lock().unwrap();
    let (context, mut app) = app();
    let mut document = Document::new(64, 48).unwrap();
    let pixels = RgbaImage::from_pixel(64, 48, image::Rgba([31, 120, 200, 255]));
    let layer = Layer::image("Source", pixels);
    document.select(layer.id, false);
    document.layers = vec![layer];
    app.sessions
        .push(Session::new(document, "Marquee".into(), None));
    click_canvas(
        &context,
        &mut app,
        Point::new(-10.0, -10.0),
        egui::Modifiers::NONE,
    );
    assert!(app.session().unwrap().document.active.is_none());
    app.set_tool(Tool::Marquee);
    drag(
        &context,
        &mut app,
        Point::new(10.0, 8.0),
        Point::new(30.0, 20.0),
        egui::Modifiers::NONE,
    );
    app.clipboard = Some((RgbaImage::new(1, 1), Point::default()));
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Copy],
        egui::Modifiers::CTRL,
    );
    let (copied, point) = app
        .clipboard
        .as_ref()
        .expect("Marquee copy must replace the old clipboard");
    assert_eq!(copied.dimensions(), (20, 12));
    assert_eq!(copied.get_pixel(0, 0).0, [31, 120, 200, 255]);
    assert_eq!(*point, Point::new(10.0, 8.0));
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Paste(String::new())],
        egui::Modifiers::CTRL,
    );
    assert!(app.error.is_none(), "{:?}", app.error);
    let document = &app.session().unwrap().document;
    assert_eq!(document.layers.len(), 2);
    let pasted = document.active().unwrap();
    assert_eq!(pasted.pixels.as_deref().unwrap().dimensions(), (20, 12));
    assert_eq!((pasted.transform.x, pasted.transform.y), (10.0, 8.0));
    app.command("undo");
    assert_eq!(app.session().unwrap().document.layers.len(), 1);
}

#[test]
fn copy_and_cut_report_missing_targets_without_changing_pixels_or_clipboard() {
    let (_, mut app) = app();
    app.dimensions = [32, 24];
    app.new_document();
    app.command("fill_fg");
    let original = app.session().unwrap().document.layers[0].pixels.clone();
    let old_pixels = RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
    app.clipboard = Some((old_pixels.clone(), Point::default()));
    let document = &mut app.session_mut().unwrap().document;
    document.active = None;
    document.selected.clear();
    app.command("copy");
    assert_eq!(
        app.error.as_deref(),
        Some("Select a layer or make a selection before copying.")
    );
    assert_eq!(app.clipboard.as_ref().unwrap().0, old_pixels);

    app.error = None;
    app.command("select_all");
    app.command("cut");
    assert_eq!(
        app.error.as_deref(),
        Some("Select a layer before cutting pixels.")
    );
    assert_eq!(app.clipboard.as_ref().unwrap().0, old_pixels);
    assert_eq!(app.session().unwrap().document.layers[0].pixels, original);
}

#[test]
fn native_clipboard_shortcuts_leave_text_editing_to_the_focused_field() {
    let (context, mut app) = app();
    app.dimensions = [8, 6];
    app.new_document();
    let layer_id = app.session().unwrap().document.active.unwrap();
    let layer_count = app.session().unwrap().document.layers.len();
    app.rename = Some((layer_id, String::new()));
    frame(&context, &mut app);
    assert!(context.wants_keyboard_input());
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Paste("Layer name".into())],
        egui::Modifiers::CTRL,
    );
    assert_eq!(app.rename.as_ref().unwrap().1, "Layer name");
    for event in [
        egui::Event::Copy,
        egui::Event::Cut,
        egui::Event::Paste(String::new()),
    ] {
        keyboard_frame(&context, &mut app, vec![event], egui::Modifiers::CTRL);
        assert!(app.clipboard.is_none());
        assert_eq!(app.session().unwrap().document.layers.len(), layer_count);
    }
}

#[test]
fn clipboard_image_pixels_create_a_centered_layer_and_undo() {
    use super::clipboard::ClipboardContent;

    let (_, mut app) = app();
    app.dimensions = [20, 16];
    app.new_document();
    app.clipboard = Some((RgbaImage::new(1, 1), Point::new(7.0, 9.0)));
    app.mask_target = true;
    let pixels = RgbaImage::from_pixel(6, 4, image::Rgba([21, 87, 163, 127]));
    app.paste_content(ClipboardContent::Image(pixels.clone()));
    let document = &app.session().unwrap().document;
    assert_eq!(document.layers.len(), 2);
    let layer = document.active().unwrap();
    assert_eq!(layer.pixels.as_deref(), Some(&pixels));
    assert_eq!((layer.transform.x, layer.transform.y), (7.0, 6.0));
    assert!(!app.mask_target);
    assert!(app.clipboard.is_none());
    app.command("undo");
    assert_eq!(app.session().unwrap().document.layers.len(), 1);
    app.command("redo");
    assert_eq!(
        app.session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .pixels
            .as_deref(),
        Some(&pixels)
    );
}

#[test]
fn clipboard_file_paste_imports_multiple_images_in_one_undo_step() {
    let temporary = tempfile::tempdir().unwrap();
    let files = [
        temporary.path().join("image one.png"),
        temporary.path().join("图片 #2.png"),
    ];
    let images = [
        RgbaImage::from_pixel(6, 4, image::Rgba([255, 0, 0, 255])),
        RgbaImage::from_pixel(4, 8, image::Rgba([0, 0, 255, 128])),
    ];
    for (path, pixels) in files.iter().zip(&images) {
        pixels.save(path).unwrap();
    }
    let original_bytes: Vec<_> = files
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    let payload = format!(
        "cut\r\n{}\r\n{}\r\n",
        url::Url::from_file_path(&files[0]).unwrap(),
        url::Url::from_file_path(&files[1]).unwrap()
    );
    let (context, mut app) = app();
    app.dimensions = [20, 16];
    app.new_document();
    app.clipboard = Some((RgbaImage::new(1, 1), Point::default()));
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Paste(payload)],
        egui::Modifiers::CTRL,
    );
    assert!(app.error.is_none(), "{:?}", app.error);
    let document = &app.session().unwrap().document;
    assert_eq!(document.layers.len(), 3);
    for (layer, pixels) in document.layers[1..].iter().zip(&images) {
        assert_eq!(layer.pixels.as_deref(), Some(pixels));
        assert_eq!(layer.transform.x, (20.0 - pixels.width() as f32) * 0.5);
        assert_eq!(layer.transform.y, (16.0 - pixels.height() as f32) * 0.5);
    }
    assert_eq!(document.layers[1].name, "image one");
    assert_eq!(document.layers[2].name, "图片 #2");
    assert!(app.clipboard.is_none());
    for (path, bytes) in files.iter().zip(&original_bytes) {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
    app.command("undo");
    assert_eq!(app.session().unwrap().document.layers.len(), 1);
    app.command("redo");
    assert_eq!(app.session().unwrap().document.layers.len(), 3);
}

#[test]
fn clipboard_psd_file_opens_a_new_session() {
    use super::clipboard::ClipboardContent;

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("clipboard.psd");
    std::fs::write(&path, minimal_psd_bytes()).unwrap();
    let (_, mut app) = app();
    app.dimensions = [20, 16];
    app.new_document();
    app.paste_content(ClipboardContent::Files(vec![path]));
    assert!(app.error.is_none(), "{:?}", app.error);
    assert_eq!(app.sessions.len(), 2);
    assert_eq!(app.session().unwrap().document.width, 2);
    assert!(app.session().unwrap().path.is_none());
}

#[test]
fn clipboard_paste_creates_a_document_when_none_is_open() {
    use super::clipboard::ClipboardContent;

    let (_, mut app) = app();
    let pixels = RgbaImage::from_pixel(6, 4, image::Rgba([42, 69, 128, 255]));
    app.paste_content(ClipboardContent::Image(pixels.clone()));
    let document = &app.session().unwrap().document;
    assert_eq!((document.width, document.height), (6, 4));
    assert_eq!(document.active().unwrap().pixels.as_deref(), Some(&pixels));
}

#[test]
fn clipboard_invalid_files_do_not_partially_paste_or_reuse_cached_pixels() {
    use super::clipboard::ClipboardContent;

    let (_, mut app) = app();
    app.dimensions = [20, 16];
    app.new_document();
    let temporary = tempfile::tempdir().unwrap();
    let good = temporary.path().join("valid.png");
    let bad = temporary.path().join("invalid.png");
    RgbaImage::new(6, 4).save(&good).unwrap();
    std::fs::write(&bad, "Not an image").unwrap();
    app.clipboard = Some((RgbaImage::new(1, 1), Point::default()));
    app.paste_content(ClipboardContent::Files(vec![good, bad]));
    assert!(app.error.as_ref().unwrap().contains("invalid.png"));
    assert_eq!(app.session().unwrap().document.layers.len(), 1);
    assert!(app.clipboard.is_none());
    assert!(!app.session().unwrap().history.dirty());

    app.clipboard = Some((RgbaImage::new(1, 1), Point::default()));
    app.paste_content(ClipboardContent::Empty);
    app.paste_content(ClipboardContent::Unavailable);
    assert_eq!(app.session().unwrap().document.layers.len(), 1);
    assert!(app.clipboard.is_none());
}

#[test]
#[ignore = "requires an isolated desktop clipboard; run under Xvfb or Xephyr"]
fn system_clipboard_images_and_files_paste_from_another_process() {
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};

    let pixels = RgbaImage::from_pixel(6, 4, image::Rgba([20, 80, 150, 127]));
    if let Some(path) = std::env::var_os("MECTOV_CLIPBOARD_TEST_IMAGE") {
        let mut clipboard = arboard::Clipboard::new().unwrap();
        for line in std::io::stdin().lock().lines() {
            match line.unwrap().as_str() {
                "image" => clipboard
                    .set_image(arboard::ImageData {
                        width: pixels.width() as usize,
                        height: pixels.height() as usize,
                        bytes: std::borrow::Cow::Borrowed(pixels.as_raw()),
                    })
                    .unwrap(),
                "file" => clipboard.set().file_list(&[PathBuf::from(&path)]).unwrap(),
                "text" => clipboard.set_text("unrelated text").unwrap(),
                "inspect" => {
                    let image = clipboard
                        .get_image()
                        .expect("Copy must replace the old file list with image pixels");
                    assert_eq!((image.width, image.height), (4, 3));
                }
                _ => panic!("unexpected clipboard test command"),
            }
            println!("clipboard ready");
            std::io::stdout().flush().unwrap();
        }
        return;
    }

    struct Producer(std::process::Child);
    impl Drop for Producer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let _clipboard_guard = CLIPBOARD_TEST_LOCK.lock().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("external 图片.png");
    pixels.save(&path).unwrap();
    let mut producer = Producer(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app::tests::system_clipboard_images_and_files_paste_from_another_process",
                "--ignored",
                "--nocapture",
            ])
            .env("MECTOV_CLIPBOARD_TEST_IMAGE", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut input = producer.0.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(producer.0.stdout.take().unwrap());
    let mut copy = |kind: &str| {
        writeln!(input, "{kind}").unwrap();
        input.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(
                output.read_line(&mut line).unwrap() > 0,
                "clipboard producer exited"
            );
            if line.trim() == "clipboard ready" {
                break;
            }
        }
    };

    let (context, mut app) = app();
    app.dimensions = [20, 16];
    app.new_document();
    for kind in ["image", "file"] {
        copy(kind);
        keyboard_frame(
            &context,
            &mut app,
            vec![egui::Event::Paste(String::new())],
            egui::Modifiers::CTRL,
        );
        assert!(app.error.is_none(), "{:?}", app.error);
        let layer = app.session().unwrap().document.active().unwrap();
        assert_eq!(layer.pixels.as_deref(), Some(&pixels));
        assert_eq!((layer.transform.x, layer.transform.y), (7.0, 6.0));
    }
    assert_eq!(app.session().unwrap().document.layers.len(), 3);
    assert_eq!(
        app.session().unwrap().document.active().unwrap().name,
        "external 图片"
    );
    app.command("paste");
    assert_eq!(app.session().unwrap().document.layers.len(), 4);
    copy("file");
    click_canvas(
        &context,
        &mut app,
        Point::new(-1.0, -1.0),
        egui::Modifiers::NONE,
    );
    assert!(app.session().unwrap().document.active.is_none());
    app.set_tool(Tool::Marquee);
    drag(
        &context,
        &mut app,
        Point::new(8.0, 6.0),
        Point::new(12.0, 9.0),
        egui::Modifiers::NONE,
    );
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Copy],
        egui::Modifiers::CTRL,
    );
    assert!(app.error.is_none(), "{:?}", app.error);
    copy("inspect");
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Paste(String::new())],
        egui::Modifiers::CTRL,
    );
    let document = &app.session().unwrap().document;
    assert_eq!(document.layers.len(), 5);
    let pasted = document.active().unwrap();
    assert_eq!(pasted.pixels.as_deref().unwrap().dimensions(), (4, 3));
    assert_eq!((pasted.transform.x, pasted.transform.y), (8.0, 6.0));
    copy("text");
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Paste("unrelated text".into())],
        egui::Modifiers::CTRL,
    );
    assert_eq!(app.session().unwrap().document.layers.len(), 5);
    assert!(app.clipboard.is_none());
}

fn frame(context: &egui::Context, app: &mut EditorApp) -> egui::FullOutput {
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(1280.0, 860.0),
            )),
            ..Default::default()
        },
        |ctx| app.show(ctx),
    );
    assert!(!output.shapes.is_empty());
    output
}

/// A click on the taller screen, at a time of the caller's choosing: egui reads
/// two clicks inside 300 ms as one double click, so a caller that wants two
/// separate taps moves the clock on.
fn tall_pointer(
    context: &egui::Context,
    app: &mut EditorApp,
    pos: Pos2,
    pressed: Option<bool>,
    time: f64,
) -> egui::FullOutput {
    let mut events = vec![egui::Event::PointerMoved(pos)];
    if let Some(pressed) = pressed {
        events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(1280.0, 1800.0),
            )),
            events,
            time: Some(time),
            ..Default::default()
        },
        |ctx| app.show(ctx),
    );
    assert!(!output.shapes.is_empty());
    output
}

/// The same frame on a taller screen, so a panel whose content is longer than
/// the usual window still draws all of it.
fn tall_frame(context: &egui::Context, app: &mut EditorApp) -> egui::FullOutput {
    let output = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(1280.0, 1800.0),
            )),
            ..Default::default()
        },
        |ctx| app.show(ctx),
    );
    assert!(!output.shapes.is_empty());
    output
}

fn pointer_frame(
    context: &egui::Context,
    app: &mut EditorApp,
    pos: Pos2,
    pressed: Option<bool>,
    modifiers: egui::Modifiers,
) -> egui::FullOutput {
    let mut events = vec![egui::Event::PointerMoved(pos)];
    if let Some(pressed) = pressed {
        events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        });
    }
    context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(1280.0, 860.0),
            )),
            events,
            modifiers,
            time: Some(app.frames as f64 / 60.0),
            ..Default::default()
        },
        |ctx| app.show(ctx),
    )
}

fn drag(
    context: &egui::Context,
    app: &mut EditorApp,
    from: Point,
    to: Point,
    modifiers: egui::Modifiers,
) {
    frame(context, app);
    let rect = app.canvas_rect.unwrap();
    let zoom = app.session().unwrap().zoom;
    let a = rect.min + Vec2::new(from.x, from.y) * zoom;
    let b = rect.min + Vec2::new(to.x, to.y) * zoom;
    pointer_frame(context, app, a, Some(true), modifiers);
    pointer_frame(context, app, a + (b - a) * 0.5, None, modifiers);
    pointer_frame(context, app, b, None, modifiers);
    pointer_frame(context, app, b, Some(false), modifiers);
}

fn click_canvas(
    context: &egui::Context,
    app: &mut EditorApp,
    point: Point,
    modifiers: egui::Modifiers,
) {
    frame(context, app);
    let pos =
        app.canvas_rect.unwrap().min + Vec2::new(point.x, point.y) * app.session().unwrap().zoom;
    pointer_frame(context, app, pos, None, modifiers);
    pointer_frame(context, app, pos, Some(true), modifiers);
    pointer_frame(context, app, pos, Some(false), modifiers);
}

fn layer_label(context: &egui::Context, app: &mut EditorApp, name: &str) -> Pos2 {
    frame(context, app)
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == name => Some(text.pos),
            _ => None,
        })
        .unwrap_or_else(|| panic!("Missing layer label: {name}"))
}

fn layer_eye(context: &egui::Context, app: &mut EditorApp, name: &str) -> Pos2 {
    let label = layer_label(context, app, name);
    frame(context, app)
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Circle(circle)
                if circle.radius == 2.0
                    && circle.center.x < label.x
                    && (label.y..label.y + 36.0).contains(&circle.center.y) =>
            {
                Some(circle.center)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("Missing visible layer eye: {name}"))
}

fn drag_pointer(
    context: &egui::Context,
    app: &mut EditorApp,
    from: Pos2,
    to: Pos2,
    modifiers: egui::Modifiers,
) {
    pointer_frame(context, app, from, Some(true), modifiers);
    pointer_frame(context, app, from + Vec2::new(0.0, 10.0), None, modifiers);
    pointer_frame(context, app, to, None, modifiers);
    pointer_frame(context, app, to, Some(false), modifiers);
}

#[test]
fn layer_visibility_eye_toggles_preview_without_selecting_and_supports_undo() {
    let (context, mut app) = app();
    app.dimensions = [32, 24];
    app.new_document();
    let bottom = Layer::image(
        "Bottom",
        RgbaImage::from_pixel(32, 24, image::Rgba([0, 0, 255, 255])),
    );
    let mut top = Layer::image(
        "Top",
        RgbaImage::from_pixel(32, 24, image::Rgba([255, 0, 0, 255])),
    );
    top.locked = true;
    let selected = bottom.id;
    let document = &mut app.session_mut().unwrap().document;
    document.layers = vec![bottom, top];
    document.select(selected, false);

    let eye = layer_eye(&context, &mut app, "Top");
    let assert_visible = |app: &EditorApp, visible: bool| {
        let session = app.session().unwrap();
        assert_eq!(session.document.layers[1].visible, visible);
        assert_eq!(session.document.active, Some(selected));
        assert_eq!(session.document.selected, HashSet::from([selected]));
        assert_eq!(
            session.composite.as_ref().unwrap().get_pixel(16, 12).0,
            if visible {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            }
        );
        assert!(app.rename.is_none());
        assert!(app.error.is_none(), "{:?}", app.error);
    };
    assert_visible(&app, true);

    pointer_frame(&context, &mut app, eye, Some(true), egui::Modifiers::NONE);
    pointer_frame(&context, &mut app, eye, Some(false), egui::Modifiers::NONE);
    assert_visible(&app, false);
    assert_eq!(
        app.session().unwrap().history.undo_name(),
        Some("Layer Visibility")
    );
    assert_eq!(app.session().unwrap().history.names().count(), 1);

    app.command("undo");
    frame(&context, &mut app);
    assert_visible(&app, true);
    app.command("redo");
    frame(&context, &mut app);
    assert_visible(&app, false);

    pointer_frame(&context, &mut app, eye, Some(true), egui::Modifiers::NONE);
    pointer_frame(&context, &mut app, eye, Some(false), egui::Modifiers::NONE);
    assert_visible(&app, true);
    assert_eq!(app.session().unwrap().history.names().count(), 2);
}

#[test]
fn collapsed_group_visibility_eye_toggles_children_in_preview() {
    let (context, mut app) = app();
    app.dimensions = [32, 24];
    app.new_document();
    let mut group = Layer::blank("Group", 32, 24);
    group.group = true;
    let group_id = group.id;
    let mut child = Layer::image(
        "Child",
        RgbaImage::from_pixel(32, 24, image::Rgba([255, 0, 0, 255])),
    );
    child.parent = Some(group_id);
    let child_id = child.id;
    let session = app.session_mut().unwrap();
    session.document.layers = vec![group, child];
    session.document.select(child_id, false);
    session.collapsed.insert(group_id);

    let eye = layer_eye(&context, &mut app, "Group");
    for visible in [false, true] {
        pointer_frame(&context, &mut app, eye, Some(true), egui::Modifiers::NONE);
        pointer_frame(&context, &mut app, eye, Some(false), egui::Modifiers::NONE);
        let session = app.session().unwrap();
        assert_eq!(session.document.layers[0].visible, visible);
        assert!(session.document.layers[1].visible);
        assert_eq!(session.document.active, Some(child_id));
        assert!(session.collapsed.contains(&group_id));
        assert_eq!(
            session.composite.as_ref().unwrap().get_pixel(16, 12).0,
            if visible { [255, 0, 0, 255] } else { [0; 4] }
        );
        assert!(app.rename.is_none());
        assert!(app.error.is_none(), "{:?}", app.error);
    }
}

#[test]
fn layer_rows_and_thumbnails_reorder_in_both_directions_and_undo() {
    let (context, mut app) = app();
    app.dimensions = [32, 24];
    app.new_document();
    let document = &mut app.session_mut().unwrap().document;
    document.layers = ["Bottom", "Middle", "Top"]
        .map(|name| Layer::blank(name, 32, 24))
        .into();
    document.select(document.layers[2].id, false);

    // Start on the row's padding, then drop below the last row.
    let top = layer_label(&context, &mut app, "Top") + Vec2::new(100.0, 30.0);
    let bottom = layer_label(&context, &mut app, "Bottom") + Vec2::new(5.0, 30.0);
    drag_pointer(&context, &mut app, top, bottom, egui::Modifiers::NONE);
    let names = |app: &EditorApp| {
        app.session()
            .unwrap()
            .document
            .layers
            .iter()
            .map(|layer| layer.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&app), ["Top", "Bottom", "Middle"]);
    assert_eq!(app.session().unwrap().history.names().count(), 1);
    app.command("undo");
    assert_eq!(names(&app), ["Bottom", "Middle", "Top"]);
    app.command("redo");
    assert_eq!(names(&app), ["Top", "Bottom", "Middle"]);

    // Start on a thumbnail and drop above the first row.
    let bottom = layer_label(&context, &mut app, "Top") + Vec2::new(-24.0, 15.0);
    let top = layer_label(&context, &mut app, "Middle") + Vec2::new(5.0, 0.0);
    drag_pointer(&context, &mut app, bottom, top, egui::Modifiers::NONE);
    assert_eq!(names(&app), ["Bottom", "Middle", "Top"]);
    assert!(app.error.is_none(), "{:?}", app.error);
}

#[test]
fn layer_drops_nest_duplicate_and_reject_descendants() {
    let (context, mut app) = app();
    app.dimensions = [32, 24];
    app.new_document();
    app.command("group");
    let folder = app.session().unwrap().document.active.unwrap();
    app.session_mut()
        .unwrap()
        .document
        .active_mut()
        .unwrap()
        .name = "Folder".into();
    let loose = Layer::blank("Loose", 32, 24);
    let loose_id = loose.id;
    app.session_mut().unwrap().document.layers.push(loose);
    let from = layer_label(&context, &mut app, "Loose") + Vec2::new(5.0, 5.0);
    let into = layer_label(&context, &mut app, "Folder") + Vec2::new(5.0, 18.0);
    drag_pointer(&context, &mut app, from, into, egui::Modifiers::ALT);
    let document = &app.session().unwrap().document;
    assert_eq!(document.layers.len(), 4);
    let copy = document.active().unwrap();
    assert_eq!(copy.name, "Loose copy");
    assert_eq!(copy.parent, Some(folder));
    assert_eq!(
        document
            .layers
            .iter()
            .find(|layer| layer.id == loose_id)
            .unwrap()
            .parent,
        None
    );
    assert!(
        layer_label(&context, &mut app, "Loose copy").y
            < layer_label(&context, &mut app, "Layer 1").y
    );

    let revision = app.session().unwrap().history.revision;
    let from = layer_label(&context, &mut app, "Folder") + Vec2::new(5.0, 5.0);
    let descendant = layer_label(&context, &mut app, "Loose copy") + Vec2::new(5.0, 5.0);
    drag_pointer(&context, &mut app, from, descendant, egui::Modifiers::NONE);
    assert_eq!(app.session().unwrap().history.revision, revision);

    // Dropping on a folder's lower edge moves the child out, below the folder.
    let from = layer_label(&context, &mut app, "Loose copy") + Vec2::new(5.0, 5.0);
    let below = layer_label(&context, &mut app, "Folder") + Vec2::new(5.0, 34.0);
    drag_pointer(&context, &mut app, from, below, egui::Modifiers::NONE);
    let document = &app.session().unwrap().document;
    assert_eq!(document.active().unwrap().parent, None);
    assert!(
        document
            .layers
            .iter()
            .position(|layer| Some(layer.id) == document.active)
            .unwrap()
            < document
                .layers
                .iter()
                .position(|layer| layer.id == folder)
                .unwrap()
    );
    document.validate().unwrap();
    assert!(app.error.is_none(), "{:?}", app.error);
}

fn canvas_layers(app: &mut EditorApp) -> [Uuid; 2] {
    app.dimensions = [100, 80];
    app.new_document();
    let mut bottom = Layer::image(
        "Bottom",
        RgbaImage::from_pixel(50, 40, image::Rgba([255; 4])),
    );
    bottom.transform.x = 10.0;
    bottom.transform.y = 10.0;
    let mut top = Layer::image("Top", RgbaImage::from_pixel(20, 20, image::Rgba([255; 4])));
    top.transform.x = 20.0;
    top.transform.y = 20.0;
    // A hole in the top layer should select the visible layer below it.
    Arc::make_mut(top.pixels.as_mut().unwrap()).put_pixel(5, 5, image::Rgba([0; 4]));
    let ids = [bottom.id, top.id];
    let document = &mut app.session_mut().unwrap().document;
    document.layers = vec![bottom, top];
    document.select(ids[0], false);
    app.snap = false;
    ids
}

#[test]
fn canvas_clicks_select_visible_layers_and_deselect_empty_space() {
    let (context, mut app) = app();
    let [bottom, top] = canvas_layers(&mut app);
    assert!(app.auto_select);
    assert!(app.ignore_transparent_pixels);
    click_canvas(
        &context,
        &mut app,
        Point::new(30.0, 30.0),
        egui::Modifiers::NONE,
    );
    assert_eq!(app.session().unwrap().document.active, Some(top));
    click_canvas(
        &context,
        &mut app,
        Point::new(25.5, 25.5),
        egui::Modifiers::NONE,
    );
    assert_eq!(app.session().unwrap().document.active, Some(bottom));
    app.session_mut().unwrap().document.layers[1].visible = false;
    click_canvas(
        &context,
        &mut app,
        Point::new(30.0, 30.0),
        egui::Modifiers::NONE,
    );
    assert_eq!(app.session().unwrap().document.active, Some(bottom));
    app.mask_target = true;
    click_canvas(
        &context,
        &mut app,
        Point::new(80.0, 65.0),
        egui::Modifiers::NONE,
    );
    assert!(app.session().unwrap().document.active.is_none());
    assert!(app.session().unwrap().document.selected.is_empty());
    assert!(!app.mask_target);
    assert!(operations::transform_box(&app.session().unwrap().document, false).is_none());

    click_canvas(
        &context,
        &mut app,
        Point::new(30.0, 30.0),
        egui::Modifiers::NONE,
    );
    assert_eq!(app.session().unwrap().document.active, Some(bottom));
    click_canvas(
        &context,
        &mut app,
        Point::new(-5.0, 30.0),
        egui::Modifiers::NONE,
    );
    assert!(app.session().unwrap().document.active.is_none());
    assert!(app.session().unwrap().document.selected.is_empty());
    assert!(!app.session().unwrap().history.dirty());
}

#[test]
fn move_tool_can_select_and_drag_through_transparent_pixels() {
    let (context, mut app) = app();
    let [bottom, top] = canvas_layers(&mut app);
    app.ignore_transparent_pixels = false;

    let hole = Point::new(25.5, 25.5);
    click_canvas(&context, &mut app, hole, egui::Modifiers::NONE);
    assert_eq!(app.session().unwrap().document.active, Some(top));

    // Starting on an unselected layer's transparent pixel selects and moves it.
    app.session_mut().unwrap().document.select(bottom, false);
    drag(
        &context,
        &mut app,
        hole,
        Point::new(30.5, 30.5),
        egui::Modifiers::NONE,
    );
    let document = &app.session().unwrap().document;
    assert_eq!(document.active, Some(top));
    assert_eq!(document.layers[0].transform.x, 10.0);
    assert_eq!(document.layers[0].transform.y, 10.0);
    assert_eq!(document.layers[1].transform.x, 25.0);
    assert_eq!(document.layers[1].transform.y, 25.0);
    app.command("undo");
    assert_eq!(app.session().unwrap().document.layers[1].transform.x, 20.0);

    app.session_mut().unwrap().document.select(bottom, false);
    app.auto_select = false;
    click_canvas(&context, &mut app, hole, egui::Modifiers::NONE);
    assert_eq!(app.session().unwrap().document.active, Some(bottom));
    click_canvas(&context, &mut app, hole, egui::Modifiers::CTRL);
    assert_eq!(app.session().unwrap().document.active, Some(top));

    let option = frame(&context, &mut app)
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == "Ignore Transparent Pixels" => {
                Some(text.pos + Vec2::splat(5.0))
            }
            _ => None,
        })
        .expect("Move tool transparency option");
    pointer_frame(&context, &mut app, option, None, egui::Modifiers::NONE);
    pointer_frame(
        &context,
        &mut app,
        option,
        Some(true),
        egui::Modifiers::NONE,
    );
    pointer_frame(
        &context,
        &mut app,
        option,
        Some(false),
        egui::Modifiers::NONE,
    );
    assert!(app.ignore_transparent_pixels);
    click_canvas(&context, &mut app, hole, egui::Modifiers::CTRL);
    assert_eq!(app.session().unwrap().document.active, Some(bottom));
}

#[test]
fn canvas_selection_preserves_multiselect_and_respects_auto_select() {
    let (context, mut app) = app();
    let [bottom, top] = canvas_layers(&mut app);
    click_canvas(
        &context,
        &mut app,
        Point::new(30.0, 30.0),
        egui::Modifiers::SHIFT,
    );
    assert_eq!(
        app.session().unwrap().document.selected,
        HashSet::from([bottom, top])
    );
    drag(
        &context,
        &mut app,
        Point::new(30.0, 30.0),
        Point::new(35.0, 35.0),
        egui::Modifiers::NONE,
    );
    let document = &app.session().unwrap().document;
    assert_eq!(document.selected, HashSet::from([bottom, top]));
    assert_eq!(document.layers[0].transform.x, 15.0);
    assert_eq!(document.layers[1].transform.x, 25.0);
    app.command("undo");
    click_canvas(
        &context,
        &mut app,
        Point::new(30.0, 30.0),
        egui::Modifiers::SHIFT,
    );
    assert_eq!(
        app.session().unwrap().document.selected,
        HashSet::from([bottom])
    );
    assert_eq!(app.session().unwrap().document.active, Some(bottom));
    click_canvas(
        &context,
        &mut app,
        Point::new(15.0, 15.0),
        egui::Modifiers::SHIFT,
    );
    assert!(app.session().unwrap().document.active.is_none());
    assert!(app.session().unwrap().document.selected.is_empty());

    app.session_mut().unwrap().document.select(bottom, false);
    app.auto_select = false;
    click_canvas(
        &context,
        &mut app,
        Point::new(30.0, 30.0),
        egui::Modifiers::NONE,
    );
    assert_eq!(app.session().unwrap().document.active, Some(bottom));
    click_canvas(
        &context,
        &mut app,
        Point::new(30.0, 30.0),
        egui::Modifiers::CTRL,
    );
    assert_eq!(app.session().unwrap().document.active, Some(top));
}

#[test]
fn empty_canvas_drags_and_panning_do_not_move_or_select_layers() {
    let (context, mut app) = app();
    let [bottom, _] = canvas_layers(&mut app);
    drag(
        &context,
        &mut app,
        Point::new(80.0, 65.0),
        Point::new(85.0, 70.0),
        egui::Modifiers::NONE,
    );
    let document = &app.session().unwrap().document;
    assert!(document.active.is_none());
    assert_eq!(document.layers[0].transform.x, 10.0);
    assert!(!app.session().unwrap().history.dirty());

    app.session_mut().unwrap().document.select(bottom, false);
    frame(&context, &mut app);
    let pos = app.canvas_rect.unwrap().min + Vec2::new(30.0, 30.0) * app.session().unwrap().zoom;
    let _ = context.run(
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        },
        |ctx| app.show(ctx),
    );
    pointer_frame(&context, &mut app, pos, Some(true), egui::Modifiers::NONE);
    pointer_frame(&context, &mut app, pos, Some(false), egui::Modifiers::NONE);
    assert_eq!(app.session().unwrap().document.active, Some(bottom));
    drag_pointer(
        &context,
        &mut app,
        pos,
        pos + Vec2::new(30.0, 20.0),
        egui::Modifiers::NONE,
    );
    assert_eq!(app.session().unwrap().document.active, Some(bottom));
    assert!(app.session().unwrap().pan.length() > 20.0);
    assert!(!app.session().unwrap().history.dirty());
}

#[test]
fn panning_preserves_saved_state_and_history_throughout_the_drag() {
    for (tool, button, space) in [
        (Tool::Move, egui::PointerButton::Middle, false),
        (Tool::Move, egui::PointerButton::Primary, true),
        (Tool::Hand, egui::PointerButton::Primary, false),
        (Tool::Clone, egui::PointerButton::Middle, false),
    ] {
        for dirty in [false, true] {
            let (context, mut app) = app();
            app.dimensions = [64, 48];
            app.new_document();
            app.command("fill_fg");
            if !dirty {
                app.session_mut().unwrap().history.mark_saved();
            }
            app.command("fill_bg");
            app.command("undo");
            app.set_tool(tool);
            frame(&context, &mut app);
            let session = app.session().unwrap();
            let pan = session.pan;
            let revision = session.history.revision;
            let undo = session.history.undo_name().map(str::to_owned);
            let redo = session.history.redo_name().map(str::to_owned);
            let document = session.document.clone();
            let title = app.window_title.clone();
            let start = app.canvas_rect.unwrap().center();
            if space {
                keyboard_frame(
                    &context,
                    &mut app,
                    vec![text_key(egui::Key::Space, egui::Modifiers::NONE)],
                    egui::Modifiers::NONE,
                );
            }
            for (delta, pressed) in [
                (Vec2::ZERO, Some(true)),
                (Vec2::new(15.0, 10.0), None),
                (Vec2::new(30.0, 20.0), None),
                (Vec2::new(30.0, 20.0), Some(false)),
            ] {
                let pos = start + delta;
                let mut events = vec![egui::Event::PointerMoved(pos)];
                if let Some(pressed) = pressed {
                    events.push(egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                keyboard_frame(&context, &mut app, events, egui::Modifiers::NONE);
                let session = app.session().unwrap();
                assert_eq!(session.history.dirty(), dirty);
                assert_eq!(session.history.revision, revision);
                assert_eq!(session.history.undo_name(), undo.as_deref());
                assert_eq!(session.history.redo_name(), redo.as_deref());
                assert_eq!(session.history.names().count(), 1);
                assert_eq!(app.window_title, title);
                assert_eq!(session.pan, pan + delta);
                assert_eq!(session.document.active, document.active);
                assert_eq!(session.document.selected, document.selected);
                assert_eq!(session.document.layers.len(), document.layers.len());
                assert_eq!(session.document.layers[0].pixels, document.layers[0].pixels);
                assert_eq!(
                    session.document.layers[0].transform,
                    document.layers[0].transform
                );
                if pressed.is_none() {
                    assert!(app.gesture.as_ref().is_some_and(|gesture| gesture.panning));
                }
            }
            assert!(app.gesture.is_none());
            assert!(app.clone_source.is_none());
            assert!(app.clone_offset.is_none());
            app.command("redo");
            assert_eq!(app.session().unwrap().history.names().count(), 2);
        }
    }
}

#[test]
fn gradient_gestures_respect_the_mask_target_and_undo() {
    for radial in [false, true] {
        for mask_target in [true, false] {
            let (context, mut app) = app();
            app.dimensions = [64, 48];
            app.new_document();
            paint::fill(
                &mut app.session_mut().unwrap().document,
                [50, 100, 150, 255],
                false,
                false,
            )
            .unwrap();
            app.command("mask");
            app.mask_target = mask_target;
            app.set_tool(Tool::Gradient);
            app.radial = radial;
            app.brush.color = [0, 0, 0, 255];
            app.background = [255; 4];
            let before = app.session().unwrap().document.active().unwrap().clone();

            drag(
                &context,
                &mut app,
                Point::new(10.5, 20.5),
                Point::new(50.5, 20.5),
                egui::Modifiers::NONE,
            );

            assert!(app.error.is_none(), "{:?}", app.error);
            let session = app.session().unwrap();
            assert_eq!(session.history.undo_name(), Some("Gradient"));
            let after = session.document.active().unwrap().clone();
            if mask_target {
                assert!(Arc::ptr_eq(
                    before.pixels.as_ref().unwrap(),
                    after.pixels.as_ref().unwrap(),
                ));
                let mask = &after.mask.as_ref().unwrap().pixels;
                assert!(mask.get_pixel(10, 20)[0] <= 1);
                assert!((127..=129).contains(&mask.get_pixel(30, 20)[0]));
                assert!(mask.get_pixel(50, 20)[0] >= 254);
            } else {
                assert!(Arc::ptr_eq(
                    &before.mask.as_ref().unwrap().pixels,
                    &after.mask.as_ref().unwrap().pixels,
                ));
                let pixels = after.pixels.as_ref().unwrap();
                assert!(pixels.get_pixel(10, 20)[0] <= 1);
                assert!((127..=129).contains(&pixels.get_pixel(30, 20)[0]));
                assert!(pixels.get_pixel(50, 20)[0] >= 254);
            }

            for (command, expected) in [("undo", before), ("redo", after)] {
                app.command(command);
                let layer = app.session().unwrap().document.active().unwrap();
                assert_eq!(layer.pixels, expected.pixels);
                assert_eq!(
                    layer.mask.as_ref().unwrap().pixels,
                    expected.mask.as_ref().unwrap().pixels,
                );
            }
        }
    }
}

#[test]
fn pointer_brush_selection_and_pixel_move_are_undoable() {
    let (context, mut app) = app();
    app.dimensions = [64, 48];
    app.new_document();
    app.brush.diameter = 4.0;
    app.brush.hardness = 1.0;
    app.brush.color = [255, 0, 0, 255];
    app.set_tool(Tool::Brush);
    drag(
        &context,
        &mut app,
        Point::new(10.0, 20.0),
        Point::new(30.0, 20.0),
        egui::Modifiers::NONE,
    );
    assert!(app.error.is_none(), "{:?}", app.error);
    assert_eq!(
        render::render(&app.session().unwrap().document)
            .get_pixel(20, 20)
            .0,
        [255, 0, 0, 255]
    );
    app.set_tool(Tool::Marquee);
    drag(
        &context,
        &mut app,
        Point::new(8.0, 16.0),
        Point::new(33.0, 25.0),
        egui::Modifiers::NONE,
    );
    assert_eq!(
        mectov::selection::bounds(app.session().unwrap().document.selection.as_ref().unwrap()),
        Some((8, 16, 33, 25))
    );
    drag(
        &context,
        &mut app,
        Point::new(15.0, 20.0),
        Point::new(35.0, 30.0),
        egui::Modifiers::CTRL,
    );
    let doc = &app.session().unwrap().document;
    assert_eq!(doc.layers.len(), 2);
    assert_eq!(render::render(doc).get_pixel(40, 30).0, [255, 0, 0, 255]);
    app.command("undo");
    assert_eq!(app.session().unwrap().document.layers.len(), 1);
    assert_eq!(
        render::render(&app.session().unwrap().document)
            .get_pixel(20, 20)
            .0,
        [255, 0, 0, 255]
    );
}

#[test]
fn smoothed_brush_stays_contiguous_and_is_one_undo_step() {
    let (context, mut app) = app();
    app.dimensions = [64, 32];
    app.new_document();
    app.brush.diameter = 3.0;
    app.brush.hardness = 1.0;
    app.brush.smoothing = 6.0;
    app.brush.color = [255, 0, 0, 255];
    app.set_tool(Tool::Brush);

    drag(
        &context,
        &mut app,
        Point::new(8.0, 16.0),
        Point::new(52.0, 16.0),
        egui::Modifiers::NONE,
    );

    assert!(app.error.is_none(), "{:?}", app.error);
    let painted = render::render(&app.session().unwrap().document);
    for x in 9..52 {
        assert_eq!(painted.get_pixel(x, 16)[3], 255, "gap at {x}");
    }
    assert_eq!(app.session().unwrap().history.undo_name(), Some("Brush"));

    app.command("undo");
    assert_eq!(
        render::render(&app.session().unwrap().document).get_pixel(30, 16)[3],
        0
    );
    app.command("redo");
    assert_eq!(
        render::render(&app.session().unwrap().document)
            .get_pixel(30, 16)
            .0,
        [255, 0, 0, 255]
    );
}

#[test]
fn transform_handles_and_control_drag_distortion_change_geometry() {
    let (context, mut app) = app();
    app.dimensions = [64, 48];
    app.new_document();
    app.command("fill_fg");
    app.snap = false;
    app.lock_ratio = false;
    drag(
        &context,
        &mut app,
        Point::new(64.0, 48.0),
        Point::new(70.0, 52.0),
        egui::Modifiers::NONE,
    );
    assert!(
        (app.session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .transform
            .width
            - 70.0)
            .abs()
            < 0.01
    );
    drag(
        &context,
        &mut app,
        Point::new(0.0, 0.0),
        Point::new(6.0, 8.0),
        egui::Modifiers::CTRL,
    );
    let transform = app.session().unwrap().document.active().unwrap().transform;
    assert!(transform.warp.is_some());
    assert!(
        transform
            .point(Point::new(0.0, 0.0))
            .distance(Point::new(6.0, 8.0))
            < 0.01
    );
}

#[test]
fn welcome_and_all_tool_panels_render_without_panics() {
    let (context, mut app) = app();
    frame(&context, &mut app);
    app.dimensions = [64, 48];
    app.new_document();
    app.command("fill_fg");
    for tool in Tool::ALL {
        app.set_tool(tool);
        frame(&context, &mut app);
    }
    assert!(app.error.is_none());
}

#[test]
fn layer_commands_and_tabs_have_independent_histories() {
    let (_, mut app) = app();
    app.dimensions = [16, 16];
    app.new_document();
    app.command("fill_fg");
    app.command("duplicate");
    assert_eq!(app.session().unwrap().document.layers.len(), 2);
    app.command("undo");
    assert_eq!(app.session().unwrap().document.layers.len(), 1);
    app.command("redo");
    assert_eq!(app.session().unwrap().document.layers.len(), 2);
    app.new_document();
    assert!(app.session().unwrap().history.undo_name().is_none());
    app.current = 0;
    assert!(app.session().unwrap().history.undo_name().is_some());
    app.command("mask");
    assert!(app.mask_target);
    app.command("delete_mask");
    assert!(!app.mask_target);
    app.session().unwrap().document.validate().unwrap();
}

#[test]
fn levels_reuses_original_histogram_source_until_dialog_closes() {
    for adjustment in [
        Adjustment::LevelsChannels {
            ranges: [mectov::color::DEFAULT_LEVELS; 4],
        },
        Adjustment::Levels {
            black: 0.0,
            gamma: 1.0,
            white: 255.0,
            output_black: 0.0,
            output_white: 255.0,
        },
    ] {
        for as_layer in [false, true] {
            let (context, mut app) = app();
            app.dimensions = [32, 24];
            app.new_document();
            app.brush.color = [180, 140, 100, 255];
            app.command("fill_fg");
            frame(&context, &mut app);
            let original = render::render(&app.session().unwrap().document);
            let expected_source = render::render_scaled(&app.session().unwrap().document, 256, 192);
            app.start_adjustment(adjustment.clone(), as_layer);
            assert!(app.effect.as_ref().unwrap().levels_source.is_none());
            frame(&context, &mut app);
            let source = app.effect.as_ref().unwrap().levels_source.as_ref().unwrap();
            assert_eq!(source, &expected_source);
            let source_pixels = source.as_ptr();

            for (channel, preview) in [(0, true), (1, true), (2, false), (3, true)] {
                let edit = app.effect.as_mut().unwrap();
                match edit.adjustment.as_mut().unwrap() {
                    Adjustment::LevelsChannels { ranges } => ranges[0][1] = 2.0,
                    Adjustment::Levels { gamma, .. } => *gamma = 2.0,
                    _ => unreachable!(),
                }
                edit.channel = channel;
                edit.preview = preview;
                edit.refresh = true;
                frame(&context, &mut app);
                pointer_frame(
                    &context,
                    &mut app,
                    Pos2::new(500.0 + channel as f32, 250.0),
                    None,
                    egui::Modifiers::NONE,
                );
                let source = app.effect.as_ref().unwrap().levels_source.as_ref().unwrap();
                assert_eq!(source.as_ptr(), source_pixels);
                assert_eq!(source, &expected_source);
                assert_eq!(
                    render::render(&app.session().unwrap().document) != original,
                    preview
                );
            }

            let apply = layer_label(&context, &mut app, "Apply") + Vec2::splat(5.0);
            pointer_frame(&context, &mut app, apply, Some(true), egui::Modifiers::NONE);
            pointer_frame(
                &context,
                &mut app,
                apply,
                Some(false),
                egui::Modifiers::NONE,
            );
            assert!(app.dialog.is_none());
            assert!(app.effect.is_none());
            let applied = render::render(&app.session().unwrap().document);
            app.command("undo");
            assert_eq!(render::render(&app.session().unwrap().document), original);
            app.command("redo");
            assert_eq!(render::render(&app.session().unwrap().document), applied);

            let expected_source = render::render_scaled(&app.session().unwrap().document, 256, 192);
            if as_layer {
                let target = app.session().unwrap().document.active.unwrap();
                app.edit_adjustment_layer(target);
            } else {
                app.start_adjustment(adjustment.clone(), false);
            }
            assert!(app.effect.as_ref().unwrap().levels_source.is_none());
            frame(&context, &mut app);
            assert_eq!(
                app.effect.as_ref().unwrap().levels_source.as_ref(),
                Some(&expected_source)
            );
            assert_ne!(
                expected_source.get_pixel(128, 96),
                &image::Rgba([180, 140, 100, 255])
            );
            let edit = app.effect.as_mut().unwrap();
            match edit.adjustment.as_mut().unwrap() {
                Adjustment::LevelsChannels { ranges } => ranges[0][1] = 3.0,
                Adjustment::Levels { gamma, .. } => *gamma = 3.0,
                _ => unreachable!(),
            }
            edit.refresh = true;
            frame(&context, &mut app);
            assert_ne!(render::render(&app.session().unwrap().document), applied);
            keyboard_frame(
                &context,
                &mut app,
                vec![text_key(egui::Key::Escape, egui::Modifiers::NONE)],
                egui::Modifiers::NONE,
            );
            assert!(app.dialog.is_none());
            assert!(app.effect.is_none());
            assert_eq!(render::render(&app.session().unwrap().document), applied);
        }
    }
}

#[test]
fn live_adjustment_cancel_restores_original_and_export_renders() {
    let (context, mut app) = app();
    app.dimensions = [16, 16];
    app.new_document();
    app.brush.color = [180, 140, 100, 255];
    app.command("fill_fg");
    let original = render::render(&app.session().unwrap().document);
    app.start_adjustment(
        Adjustment::Exposure {
            exposure: -2.0,
            offset: 0.0,
            gamma: 1.0,
        },
        false,
    );
    frame(&context, &mut app);
    assert_ne!(render::render(&app.session().unwrap().document), original);
    let session = app.session_mut().unwrap();
    session.history.cancel(&mut session.document);
    assert_eq!(render::render(&app.session().unwrap().document), original);
    app.effect = None;
    app.dialog = Some(Dialog::Export);
    frame(&context, &mut app);
    assert!(app.export_texture.is_some());
}

#[test]
fn background_jobs_commit_once_and_cancel_without_losing_edits() {
    use std::sync::atomic::Ordering;
    let (_, mut app) = app();
    app.dimensions = [8, 8];
    app.new_document();
    app.command("fill_fg");
    let revision = app.session().unwrap().history.revision;
    app.start_job("Worker edit", |document, _| {
        document.layers[0].name = "Worker result".into();
        Ok(())
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.job.is_some() {
        app.poll_job();
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(
        app.session().unwrap().document.layers[0].name,
        "Worker result"
    );
    assert_eq!(app.session().unwrap().history.revision, revision + 1);
    app.command("undo");
    assert_eq!(app.session().unwrap().document.layers[0].name, "Layer 1");

    app.start_job("Cancelled edit", |document, cancel| {
        while !cancel.load(Ordering::Relaxed) {
            std::thread::yield_now();
        }
        document.layers.clear();
        Ok(())
    });
    app.job
        .as_ref()
        .unwrap()
        .cancel
        .store(true, Ordering::Relaxed);
    while app.job.is_some() {
        app.poll_job();
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(app.session().unwrap().document.layers.len(), 1);
    assert_eq!(app.session().unwrap().history.revision, revision);
}

#[test]
fn copying_layers_between_projects_keeps_source_and_undoes_in_destination() {
    let (_, mut app) = app();
    app.dimensions = [16, 16];
    app.new_document();
    app.command("fill_fg");
    app.command("group");
    let source = &app.session().unwrap().document;
    let drag = LayerDrag {
        project: source.id,
        layer: source.active.unwrap(),
    };
    let source_pixels = render::render(source);
    app.new_document();
    app.copy_layer_to_project(drag, 1);
    assert_eq!(app.sessions[1].document.layers.len(), 3);
    assert_eq!(render::render(&app.sessions[1].document), source_pixels);
    assert_eq!(render::render(&app.sessions[0].document), source_pixels);
    app.command("undo");
    assert_eq!(app.sessions[1].document.layers.len(), 1);
    assert_eq!(app.sessions[0].document.layers.len(), 2);
}

fn has_command(
    output: &egui::FullOutput,
    predicate: impl Fn(&egui::ViewportCommand) -> bool,
) -> bool {
    output
        .viewport_output
        .values()
        .any(|viewport| viewport.commands.iter().any(&predicate))
}

#[test]
fn idle_window_stops_requesting_repaints() {
    for with_document in [false, true] {
        let (context, mut app) = app();
        if with_document {
            app.dimensions = [64, 48];
            app.new_document();
        }
        // Let layout, font uploads, and opening animations settle.
        for _ in 0..30 {
            frame(&context, &mut app);
        }
        let output = frame(&context, &mut app);
        assert_eq!(
            output.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
            std::time::Duration::MAX,
            "Idle window kept requesting repaints (document: {with_document})"
        );
    }
}

#[test]
fn window_title_updates_on_document_and_dirty_state_changes() {
    let (context, mut app) = app();
    let expect_title = |app: &mut EditorApp, title: &str| {
        let output = frame(&context, app);
        assert!(has_command(&output, |command| matches!(
            command,
            egui::ViewportCommand::Title(value) if value == title
        )));
        let output = frame(&context, app);
        assert!(!has_command(&output, |command| matches!(
            command,
            egui::ViewportCommand::Title(_)
        )));
    };

    expect_title(&mut app, "mectov");
    app.dimensions = [64, 48];
    app.new_document();
    app.session_mut().unwrap().title = "Photo".into();
    expect_title(&mut app, "Photo —  mectov");
    app.command("fill_fg");
    expect_title(&mut app, "Photo • —  mectov");
    app.command("undo");
    expect_title(&mut app, "Photo —  mectov");
    app.command("redo");
    expect_title(&mut app, "Photo • —  mectov");
    app.session_mut().unwrap().history.mark_saved();
    expect_title(&mut app, "Photo —  mectov");

    app.new_document();
    expect_title(&mut app, "Untitled —  mectov");
    app.current = 0;
    expect_title(&mut app, "Photo —  mectov");
    app.session_mut().unwrap().title = "Saved photo".into();
    expect_title(&mut app, "Saved photo —  mectov");
    app.sessions.clear();
    expect_title(&mut app, "mectov");
}

#[test]
fn client_titlebar_moves_resizes_and_preserves_unsaved_close_flow() {
    let (context, mut app) = app();
    frame(&context, &mut app);
    frame(&context, &mut app);
    let output = pointer_frame(
        &context,
        &mut app,
        Pos2::new(950.0, 20.0),
        Some(true),
        egui::Modifiers::NONE,
    );
    assert!(!has_command(&output, |c| matches!(
        c,
        egui::ViewportCommand::StartDrag
    )));
    let output = pointer_frame(
        &context,
        &mut app,
        Pos2::new(970.0, 25.0),
        None,
        egui::Modifiers::NONE,
    );
    assert!(has_command(&output, |c| matches!(
        c,
        egui::ViewportCommand::StartDrag
    )));
    pointer_frame(
        &context,
        &mut app,
        Pos2::new(970.0, 25.0),
        Some(false),
        egui::Modifiers::NONE,
    );

    // Resize from the undecorated left edge.
    pointer_frame(
        &context,
        &mut app,
        Pos2::new(1.0, 400.0),
        None,
        egui::Modifiers::NONE,
    );
    let output = pointer_frame(
        &context,
        &mut app,
        Pos2::new(1.0, 400.0),
        Some(true),
        egui::Modifiers::NONE,
    );
    assert!(has_command(&output, |c| matches!(
        c,
        egui::ViewportCommand::BeginResize(egui::ResizeDirection::West)
    )));
    pointer_frame(
        &context,
        &mut app,
        Pos2::new(1.0, 400.0),
        Some(false),
        egui::Modifiers::NONE,
    );

    for (x, maximize) in [(61.0, true), (41.0, false)] {
        pointer_frame(
            &context,
            &mut app,
            Pos2::new(x, 20.0),
            None,
            egui::Modifiers::NONE,
        );
        pointer_frame(
            &context,
            &mut app,
            Pos2::new(x, 20.0),
            Some(true),
            egui::Modifiers::NONE,
        );
        let output = pointer_frame(
            &context,
            &mut app,
            Pos2::new(x, 20.0),
            Some(false),
            egui::Modifiers::NONE,
        );
        assert!(has_command(&output, |c| if maximize {
            matches!(c, egui::ViewportCommand::Maximized(true))
        } else {
            matches!(c, egui::ViewportCommand::Minimized(true))
        }));
    }

    app.dimensions = [16, 16];
    app.new_document();
    app.command("fill_fg");
    frame(&context, &mut app);
    pointer_frame(
        &context,
        &mut app,
        Pos2::new(21.0, 20.0),
        Some(true),
        egui::Modifiers::NONE,
    );
    let output = pointer_frame(
        &context,
        &mut app,
        Pos2::new(21.0, 20.0),
        Some(false),
        egui::Modifiers::NONE,
    );
    assert!(app.close_app);
    assert!(!has_command(&output, |c| matches!(
        c,
        egui::ViewportCommand::Close
    )));
    assert_eq!(app.sessions.len(), 1);
}

#[test]
fn titlebar_double_click_toggles_maximize_without_starting_a_drag() {
    for maximized in [false, true] {
        for x in [640.0, 950.0] {
            let (context, mut app) = app();
            frame(&context, &mut app);
            frame(&context, &mut app);
            for (index, pressed) in [true, false, true, false].into_iter().enumerate() {
                let pos = Pos2::new(x, 20.0);
                let output = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            Pos2::ZERO,
                            Vec2::new(1280.0, 860.0),
                        )),
                        viewports: [(
                            egui::ViewportId::ROOT,
                            egui::ViewportInfo {
                                maximized: Some(maximized),
                                ..Default::default()
                            },
                        )]
                        .into_iter()
                        .collect(),
                        events: vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                        time: Some(1.0 + index as f64 * 0.05),
                        ..Default::default()
                    },
                    |ctx| app.show(ctx),
                );
                assert!(!has_command(&output, |c| matches!(
                    c,
                    egui::ViewportCommand::StartDrag
                )));
                let toggles: Vec<_> = output
                    .viewport_output
                    .values()
                    .flat_map(|viewport| &viewport.commands)
                    .filter_map(|command| match command {
                        egui::ViewportCommand::Maximized(value) => Some(*value),
                        _ => None,
                    })
                    .collect();
                if index == 3 {
                    assert_eq!(toggles, vec![!maximized]);
                } else {
                    assert!(toggles.is_empty());
                }
            }
        }
    }
}

#[test]
fn custom_controls_keep_keyboard_input_and_disabled_behavior() {
    let context = egui::Context::default();
    theme::apply(&context);
    let mut value = 0.5_f32;
    let mut enabled = true;
    let mut slider_id = egui::Id::NULL;
    let mut draw = |events: Vec<egui::Event>, enabled: bool| {
        let _ = context.run(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        let response =
                            ui.add(widgets::Slider::new(&mut value, 0.0..=1.0).percentage());
                        slider_id = response.id;
                        response.request_focus();
                    });
                });
            },
        );
        value
    };
    draw(Vec::new(), enabled);
    let key = egui::Event::Key {
        key: egui::Key::ArrowRight,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let changed = draw(vec![key.clone()], enabled);
    assert!(changed > 0.5 && changed <= 1.0);
    enabled = false;
    let unchanged = draw(vec![key], enabled);
    assert_eq!(unchanged, changed);
}

fn wheel_events(pos: Pos2, delta: Vec2, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta,
            modifiers,
        },
    ]
}

fn wheel_control_frame(
    context: &egui::Context,
    events: Vec<egui::Event>,
    mut control: impl FnMut(&mut egui::Ui) -> egui::Response,
) -> (egui::Response, Vec2) {
    let mut result = None;
    let _ = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(400.0, 160.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let scroll = egui::ScrollArea::both().show(ui, |ui| {
                    let response = control(ui);
                    ui.allocate_space(Vec2::splat(800.0));
                    response
                });
                result = Some((scroll.inner, scroll.state.offset));
            });
        },
    );
    result.unwrap()
}

#[test]
fn number_wheel_changes_clamps_and_consumes_panel_scrolling() {
    let context = egui::Context::default();
    let mut value = 10_u32;
    let mut draw = |events, enabled| {
        let (response, offset) = wheel_control_frame(&context, events, |ui| {
            ui.add_enabled(enabled, widgets::Number::new(&mut value).range(0..=20))
        });
        (value, response, offset)
    };
    let (_, response, _) = draw(Vec::new(), true);
    let pos = response.rect.center();
    draw(vec![egui::Event::PointerMoved(pos)], true);
    for (delta, expected, changed) in [
        (1.0, 11, true),
        (-2.0, 9, true),
        (100.0, 20, true),
        (1.0, 20, false),
        (-100.0, 0, true),
        (-1.0, 0, false),
    ] {
        let (value, response, offset) = draw(
            wheel_events(pos, Vec2::new(0.0, delta), egui::Modifiers::NONE),
            true,
        );
        assert_eq!(value, expected);
        assert_eq!(response.changed(), changed);
        assert_eq!(offset, Vec2::ZERO);
    }
    // The smoothing tail must neither edit again nor scroll the containing panel.
    for _ in 0..30 {
        let (value, response, offset) = draw(Vec::new(), true);
        assert_eq!(value, 0);
        assert!(!response.changed());
        assert_eq!(offset, Vec2::ZERO);
    }
    let (value, response, _) = draw(
        wheel_events(pos, Vec2::new(0.0, 1.0), egui::Modifiers::NONE),
        false,
    );
    assert_eq!(value, 0);
    assert!(!response.changed());
}

#[test]
fn slider_wheel_matches_number_steps_and_preserves_horizontal_scrolling() {
    for (range, percentage, logarithmic, initial, step) in [
        (0.0..=1.0, true, false, 0.5_f64, 0.01),
        (-5.0..=5.0, false, false, 0.0, 0.01),
        (0.1..=100.0, false, true, 10.0, 1.0),
    ] {
        let context = egui::Context::default();
        let mut value = initial;
        let mut draw = |events| {
            let (response, offset) = wheel_control_frame(&context, events, |ui| {
                let slider =
                    widgets::Slider::new(&mut value, range.clone()).logarithmic(logarithmic);
                ui.add(if percentage {
                    slider.percentage()
                } else {
                    slider
                })
            });
            (value, response, offset)
        };
        let (_, response, _) = draw(Vec::new());
        let rail = egui::pos2(response.rect.left() + 10.0, response.rect.center().y);
        let field = egui::pos2(response.rect.right() - 10.0, response.rect.center().y);
        draw(vec![egui::Event::PointerMoved(rail)]);
        let (value, response, offset) = draw(wheel_events(
            rail,
            Vec2::new(0.0, 1.0),
            egui::Modifiers::NONE,
        ));
        assert!((value - initial - step).abs() < 1e-6);
        assert!(response.changed());
        assert_eq!(offset, Vec2::ZERO);
        draw(vec![egui::Event::PointerMoved(field)]);
        let (value, response, offset) = draw(wheel_events(
            field,
            Vec2::new(0.0, -1.0),
            egui::Modifiers::NONE,
        ));
        assert!((value - initial).abs() < 1e-6);
        assert!(response.changed());
        assert_eq!(offset, Vec2::ZERO);
        let (value, response, offset) = draw(wheel_events(
            field,
            Vec2::new(-1.0, 0.0),
            egui::Modifiers::NONE,
        ));
        assert!((value - initial).abs() < 1e-6);
        assert!(!response.changed());
        assert!(offset.x > 0.0);
    }
}

/// One click in the given spot. A double-click is two of these in a row, in
/// separate frames, which is how a pointer really delivers it. Each scenario
/// gets its own context, because egui counts clicks per context and a third
/// click in the same spot is a triple-click rather than a double-click.
fn clicks(count: usize, pos: Pos2) -> Vec<egui::Event> {
    let mut events = Vec::new();
    for _ in 0..count {
        events.push(egui::Event::PointerMoved(pos));
        events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
    }
    events
}

/// Draws one slider in a `0.0..=1.0` range and reports the value, the response,
/// and the two spots a test clicks: the track and the number beside it.
fn unit_slider(
    context: &egui::Context,
    value: &mut f64,
    reset: Option<f64>,
    events: Vec<egui::Event>,
) -> (f64, egui::Response, Pos2, Pos2) {
    let mut spots = None;
    let (response, _) = wheel_control_frame(context, events, |ui| {
        let mut slider = widgets::Slider::new(value, 0.0..=1.0);
        if let Some(reset) = reset {
            slider = slider.reset_to(reset);
        }
        let response = ui.add(slider);
        spots = Some((
            egui::pos2(response.rect.left() + 10.0, response.rect.center().y),
            egui::pos2(response.rect.right() - 10.0, response.rect.center().y),
        ));
        response
    });
    let (track, field) = spots.unwrap();
    (*value, response, track, field)
}

/// The same, for a range that holds no zero.
fn narrow_slider(
    context: &egui::Context,
    value: &mut f64,
    reset: Option<f64>,
    events: Vec<egui::Event>,
) -> (f64, egui::Response, Pos2) {
    let mut track = None;
    let (response, _) = wheel_control_frame(context, events, |ui| {
        let mut slider = widgets::Slider::new(value, 10.0..=100.0);
        if let Some(reset) = reset {
            slider = slider.reset_to(reset);
        }
        let response = ui.add(slider);
        track = Some(egui::pos2(
            response.rect.left() + 10.0,
            response.rect.center().y,
        ));
        response
    });
    (*value, response, track.unwrap())
}

#[test]
fn a_double_click_resets_a_slider_to_its_neutral_value() {
    // A slider with zero in its range returns to zero.
    let context = egui::Context::default();
    let mut value = 0.5_f64;
    let (after, _, track, field) = unit_slider(&context, &mut value, None, Vec::new());
    assert_eq!(after, 0.5);
    unit_slider(&context, &mut value, None, clicks(1, track));
    let (after, response, _, _) = unit_slider(&context, &mut value, None, clicks(1, track));
    assert_eq!(after, 0.0, "a double-click returns it to zero");
    assert!(response.changed());
    // The number beside the track keeps its own double-click, for selecting text.
    let field_context = egui::Context::default();
    let (after, response, _, _) =
        unit_slider(&field_context, &mut value, Some(0.25), clicks(2, field));
    assert_eq!(after, 0.0, "a double-click on the number does not reset");
    assert!(!response.changed());
    // A single click is a drag or a value change, never a reset.
    let single = egui::Context::default();
    let (after, response, _, _) = unit_slider(&single, &mut value, None, clicks(1, track));
    assert_eq!(after, 0.0, "one click is not a reset");
    assert!(!response.changed());
}

#[test]
fn a_slider_whose_range_holds_no_zero_resets_only_to_a_named_value() {
    let context = egui::Context::default();
    let mut narrow = 20.0_f64;
    let (after, _, track) = narrow_slider(&context, &mut narrow, None, Vec::new());
    assert_eq!(after, 20.0);
    narrow_slider(&context, &mut narrow, None, clicks(1, track));
    let (after, response, _) = narrow_slider(&context, &mut narrow, None, clicks(1, track));
    assert_eq!(after, 20.0, "nothing to reset to, so nothing happens");
    assert!(!response.changed());

    let named = egui::Context::default();
    narrow_slider(&named, &mut narrow, Some(25.0), clicks(1, track));
    let (after, response, _) = narrow_slider(&named, &mut narrow, Some(25.0), clicks(1, track));
    assert_eq!(after, 25.0, "the named value is the neutral one");
    assert!(response.changed());

    let outside = egui::Context::default();
    narrow_slider(&outside, &mut narrow, Some(1_000.0), clicks(1, track));
    let (after, response, _) =
        narrow_slider(&outside, &mut narrow, Some(1_000.0), clicks(1, track));
    assert_eq!(after, 25.0, "a named value outside the range is ignored");
    assert!(!response.changed());
}

#[test]
fn number_wheel_accumulates_small_deltas_and_keeps_focused_text_current() {
    let context = egui::Context::default();
    let mut value = 1.0_f64;
    let mut draw = |events| {
        let (response, _) = wheel_control_frame(&context, events, |ui| {
            ui.add(widgets::Number::new(&mut value).speed(0.01).max_decimals(2))
        });
        (value, response)
    };
    let (_, response) = draw(Vec::new());
    response.request_focus();
    let pos = response.rect.center();
    draw(vec![egui::Event::PointerMoved(pos)]);
    let line_height = context.options(|options| options.input_options.line_scroll_speed);
    for index in 0..10 {
        let (value, response) = draw(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, line_height / 10.0),
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        assert_eq!(value, if index == 9 { 1.01 } else { 1.0 });
        assert_eq!(response.changed(), index == 9);
    }
    let (value, response) = draw(Vec::new());
    assert_eq!(value, 1.01);
    assert_eq!(
        context
            .data(|data| data.get_temp::<String>(response.id))
            .as_deref(),
        Some("1.01"),
    );
    let (value, response) = draw(wheel_events(
        pos + Vec2::new(100.0, 0.0),
        Vec2::new(0.0, 1.0),
        egui::Modifiers::NONE,
    ));
    assert_eq!(value, 1.01);
    assert!(!response.changed());
}

#[test]
fn canvas_wheel_pans_horizontally_and_keeps_vertical_zoom_anchored() {
    for (delta, modifiers) in [
        (Vec2::new(-1.0, 0.0), egui::Modifiers::NONE),
        (Vec2::new(0.0, -1.0), egui::Modifiers::SHIFT),
        (Vec2::new(0.0, 1.0), egui::Modifiers::NONE),
    ] {
        let (context, mut app) = app();
        app.dimensions = [32, 24];
        app.new_document();
        frame(&context, &mut app);
        let pos = app.canvas_rect.unwrap().center() + Vec2::new(30.0, 20.0);
        pointer_frame(&context, &mut app, pos, None, modifiers);
        let before = app.session().unwrap();
        let zoom = before.zoom;
        let pan = before.pan;
        let point = (pos - app.canvas_rect.unwrap().min) / zoom;
        let _ = context.run(
            egui::RawInput {
                events: wheel_events(pos, delta, modifiers),
                modifiers,
                ..Default::default()
            },
            |ctx| app.show(ctx),
        );
        frame(&context, &mut app);
        let after = app.session().unwrap();
        if modifiers.shift || delta.x != 0.0 {
            assert!(after.pan.x < pan.x);
            assert_eq!(after.pan.y, pan.y);
            assert_eq!(after.zoom, zoom);
        } else {
            assert!(after.zoom > zoom);
            // Rendering uses the previous frame's view; settle the smoothing first.
            for _ in 0..30 {
                frame(&context, &mut app);
            }
            let after_point = (pos - app.canvas_rect.unwrap().min) / app.session().unwrap().zoom;
            assert!((after_point - point).length() < 0.001);
        }
    }
}

#[test]
fn floating_panels_stay_bounded_at_minimum_window_size() {
    let (context, mut app) = app();
    app.dimensions = [32, 24];
    app.new_document();
    app.command("fill_fg");
    app.command("levels");
    for _ in 0..5 {
        frame(&context, &mut app);
    }
    let full = context
        .memory(|memory| memory.area_rect(egui::Id::new("Levels")))
        .unwrap();
    assert!(
        full.height() > 530.0,
        "Levels should expand before scrolling: {full:?}"
    );
    for panel in ["levels", "hue", "curves", "export", "new", "text"] {
        app.dialog = None;
        app.effect = None;
        app.export_format = "jpg".into();
        if panel == "text" {
            app.start_text(None, Point::default());
            app.text_edit.as_mut().unwrap().style.content = "A long text document\n".repeat(60);
        } else {
            app.command(panel);
        }
        for _ in 0..5 {
            let _ = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        Pos2::ZERO,
                        Vec2::new(850.0, 560.0),
                    )),
                    ..Default::default()
                },
                |ctx| app.show(ctx),
            );
        }
        let title = match panel {
            "levels" => "Levels",
            "hue" => "Hue/Saturation",
            "curves" => "Curves",
            "export" => "Export image",
            "text" => "Text",
            _ => "New canvas",
        };
        let rect = context
            .memory(|memory| memory.area_rect(egui::Id::new(title)))
            .expect(title);
        assert!(rect.width() < 740.0, "{title} grew to {rect:?}");
        assert!(rect.height() <= 542.0, "{title} is too tall: {rect:?}");
        assert!(
            rect.top() >= 0.0 && rect.bottom() <= 560.0,
            "{title} clipped vertically: {rect:?}"
        );
        assert!(
            rect.left() >= 0.0 && rect.right() <= 850.0,
            "{title} clipped: {rect:?}"
        );
    }
}

#[test]
fn floating_panel_title_remains_draggable() {
    let (context, mut app) = app();
    app.dimensions = [16, 16];
    app.new_document();
    app.command("hue");
    for _ in 0..3 {
        frame(&context, &mut app);
    }
    let id = egui::Id::new("Hue/Saturation");
    let before = context.memory(|memory| memory.area_rect(id)).unwrap();
    let start = before.center_top() + Vec2::new(0.0, 15.0);
    let end = start + Vec2::new(50.0, 30.0);
    pointer_frame(&context, &mut app, start, None, egui::Modifiers::NONE);
    pointer_frame(&context, &mut app, start, Some(true), egui::Modifiers::NONE);
    pointer_frame(&context, &mut app, end, None, egui::Modifiers::NONE);
    pointer_frame(&context, &mut app, end, Some(false), egui::Modifiers::NONE);
    let after = context.memory(|memory| memory.area_rect(id)).unwrap();
    assert!(
        (after.min - before.min).length() > 20.0,
        "Panel didn't move: {before:?} → {after:?}"
    );
}

#[test]
fn double_click_raw_layer_opens_develop_and_rasterization_is_undoable() {
    let (context, mut app) = app();
    app.dimensions = [80, 60];
    app.new_document();
    let doc = &mut app.session_mut().unwrap().document;
    let layer = doc.active_mut().unwrap();
    layer.name = "Camera RAW".into();
    layer.pixels = Some(Arc::new(RgbaImage::from_pixel(
        80,
        60,
        image::Rgba([100, 90, 80, 255]),
    )));
    layer.raw = Some(mectov::raw::RawAsset {
        filename: "camera.NEF".into(),
        bytes: Arc::new(vec![1, 2, 3]),
        metadata: mectov::raw::RawMetadata {
            width: 80,
            height: 60,
            ..Default::default()
        },
        settings: mectov::raw::DevelopSettings::default(),
    });
    let pos = layer_label(&context, &mut app, "Camera RAW") + Vec2::new(5.0, 5.0);
    for _ in 0..2 {
        pointer_frame(&context, &mut app, pos, Some(true), egui::Modifiers::NONE);
        pointer_frame(&context, &mut app, pos, Some(false), egui::Modifiers::NONE);
    }
    assert!(app.develop.is_some());
    assert!(app.rename.is_none());
    app.cancel_develop();
    // Let the previous double-click expire before testing a second target.
    for _ in 0..40 {
        frame(&context, &mut app);
    }
    let output = frame(&context, &mut app);
    let session = app.session().unwrap();
    let thumbnail_id = session.thumbnails[&(session.document.active.unwrap(), false)].id();
    let thumbnail = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id == thumbnail_id => {
                Some(mesh.calc_bounds().center())
            }
            _ => None,
        })
        .expect("RAW thumbnail is visible");
    pointer_frame(&context, &mut app, thumbnail, None, egui::Modifiers::NONE);
    for _ in 0..2 {
        pointer_frame(
            &context,
            &mut app,
            thumbnail,
            Some(true),
            egui::Modifiers::NONE,
        );
        pointer_frame(
            &context,
            &mut app,
            thumbnail,
            Some(false),
            egui::Modifiers::NONE,
        );
    }
    assert!(app.develop.is_some());
    app.cancel_develop();
    app.command("rasterize_raw");
    assert!(
        app.session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .raw
            .is_none()
    );
    app.command("undo");
    assert!(
        app.session()
            .unwrap()
            .document
            .active()
            .unwrap()
            .raw
            .is_some()
    );
}

/// Compositor 1.2.1 selects the layer under the pointer by default and picks the foreground layer
/// when a stack is clicked, including a subject sitting on a full-canvas background.
#[test]
fn auto_select_picks_the_foreground_layer_over_a_full_canvas_background() {
    let (context, mut app) = app();
    app.dimensions = [100, 80];
    app.new_document();
    let background = Layer::image(
        "Background",
        RgbaImage::from_pixel(100, 80, image::Rgba([40, 60, 90, 255])),
    );
    let mut subject = Layer::image(
        "Subject",
        RgbaImage::from_pixel(20, 20, image::Rgba([200, 30, 30, 255])),
    );
    subject.transform.x = 40.0;
    subject.transform.y = 30.0;
    let ids = [background.id, subject.id];
    let document = &mut app.session_mut().unwrap().document;
    document.layers = vec![background, subject];
    document.select(ids[0], false);
    app.snap = false;
    click_canvas(
        &context,
        &mut app,
        Point::new(50.0, 40.0),
        egui::Modifiers::NONE,
    );
    assert_eq!(app.session().unwrap().document.active, Some(ids[1]));
    click_canvas(
        &context,
        &mut app,
        Point::new(5.0, 5.0),
        egui::Modifiers::NONE,
    );
    assert_eq!(app.session().unwrap().document.active, Some(ids[0]));
}

/// Shift-+ / Shift− steps along the blend menu's Photoshop order as one undo step.
#[test]
fn blend_mode_cycles_along_the_menu_order() {
    let (_, mut app) = app();
    let [bottom, _] = canvas_layers(&mut app);
    app.session_mut().unwrap().document.select(bottom, false);
    let step = |app: &mut EditorApp, command: &str| {
        app.command(command);
        app.session().unwrap().document.active().unwrap().blend
    };
    assert_eq!(step(&mut app, "blend_next"), BlendMode::Darken);
    assert_eq!(step(&mut app, "blend_next"), BlendMode::Multiply);
    assert_eq!(step(&mut app, "blend_next"), BlendMode::ColorBurn);
    assert_eq!(step(&mut app, "blend_next"), BlendMode::LinearBurn);
    app.command("undo");
    app.command("undo");
    app.command("undo");
    app.command("undo");
    assert_eq!(
        app.session().unwrap().document.active().unwrap().blend,
        BlendMode::Normal
    );
    // The last mode wraps back around to the first.
    app.command("blend_prev");
    assert_eq!(
        app.session().unwrap().document.active().unwrap().blend,
        BlendMode::Luminosity
    );
}

#[test]
fn remapped_command_and_tool_shortcuts_dispatch_without_reacting_to_old_keys() {
    let (context, mut app) = app();
    app.set_shortcut_binding(
        ShortcutAction::Command("new"),
        shortcuts::Shortcut::new(egui::Key::K, true, false, false),
    )
    .unwrap();
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::N, egui::Modifiers::CTRL)],
        egui::Modifiers::CTRL,
    );
    assert!(app.dialog != Some(Dialog::New));
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::K, egui::Modifiers::CTRL)],
        egui::Modifiers::CTRL,
    );
    assert!(app.dialog == Some(Dialog::New));
}

#[test]
fn remapped_tool_shortcut_updates_the_tool_and_hover_label() {
    let (context, mut app) = app();
    app.set_shortcut_binding(
        ShortcutAction::Tool(Tool::Brush),
        shortcuts::Shortcut::new(egui::Key::K, false, false, true),
    )
    .unwrap();
    assert_eq!(app.tool_shortcut(Tool::Brush), "Alt+K");
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::B, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.tool, Tool::Move);
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::K, egui::Modifiers::ALT)],
        egui::Modifiers::ALT,
    );
    assert_eq!(app.tool, Tool::Brush);
}

#[test]
fn shortcut_capture_updates_a_binding_and_keeps_conflicts_active() {
    let (context, mut app) = app();
    app.dialog = Some(Dialog::Shortcuts);
    app.shortcut_capture = Some(ShortcutAction::Command("new"));
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::K, egui::Modifiers::CTRL)],
        egui::Modifiers::CTRL,
    );
    assert!(app.shortcut_capture.is_none());
    assert_eq!(
        app.command_shortcut_labels().get("new").map(String::as_str),
        Some("Ctrl+K")
    );

    app.shortcut_capture = Some(ShortcutAction::Command("open"));
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::S, egui::Modifiers::CTRL)],
        egui::Modifiers::CTRL,
    );
    assert!(app.shortcut_capture.is_some());
    assert!(app.shortcut_error.is_some());
}

#[test]
fn remapped_native_copy_uses_the_new_clipboard_chord_only() {
    let _clipboard_guard = CLIPBOARD_TEST_LOCK.lock().unwrap();
    let (context, mut app) = app();
    app.dimensions = [16, 12];
    app.new_document();
    app.command("fill_fg");
    app.command("select_all");
    app.set_shortcut_binding(
        ShortcutAction::Command("copy"),
        shortcuts::Shortcut::new(egui::Key::C, true, false, true),
    )
    .unwrap();
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Copy],
        egui::Modifiers::CTRL,
    );
    assert!(app.clipboard.is_none());
    keyboard_frame(
        &context,
        &mut app,
        vec![egui::Event::Copy],
        egui::Modifiers {
            ctrl: true,
            alt: true,
            ..egui::Modifiers::NONE
        },
    );
    assert!(app.clipboard.is_some());
}

#[test]
fn remapped_delete_chord_disables_the_old_delete_key() {
    let (context, mut app) = app();
    app.dimensions = [16, 12];
    app.new_document();
    app.set_shortcut_binding(
        ShortcutAction::Command("clear_or_delete"),
        shortcuts::Shortcut::new(egui::Key::F2, false, false, false),
    )
    .unwrap();
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::Delete, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.session().unwrap().document.layers.len(), 1);
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(egui::Key::F2, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert!(app.session().unwrap().document.layers.is_empty());
}

#[test]
fn zoom_in_accepts_the_physical_shift_plus_chord() {
    let (context, mut app) = app();
    app.new_document();
    let before = app.session().unwrap().zoom;
    keyboard_frame(
        &context,
        &mut app,
        vec![text_key(
            egui::Key::Plus,
            egui::Modifiers {
                ctrl: true,
                shift: true,
                ..egui::Modifiers::NONE
            },
        )],
        egui::Modifiers {
            ctrl: true,
            shift: true,
            ..egui::Modifiers::NONE
        },
    );
    assert!(app.session().unwrap().zoom > before);
}

#[test]
fn reserved_fixed_keys_cannot_be_assigned_to_commands() {
    let (_, mut app) = app();
    assert!(
        app.set_shortcut_binding(
            ShortcutAction::Command("new"),
            shortcuts::Shortcut::new(egui::Key::Escape, false, false, false),
        )
        .is_err()
    );
}

#[test]
fn shortcut_settings_are_saved_with_the_global_tool_settings() {
    let (_, mut editor) = app();
    editor
        .set_shortcut_binding(
            ShortcutAction::Command("save"),
            shortcuts::Shortcut::new(egui::Key::K, true, false, false),
        )
        .unwrap();
    let mut storage = TestStorage::default();
    eframe::App::save(&mut editor, &mut storage);
    let stored = eframe::get_value::<ShortcutSettings>(&storage, shortcuts::STORAGE_KEY).unwrap();
    assert!(stored.is_valid());
    assert_eq!(
        editor
            .command_shortcut_labels()
            .get("save")
            .map(String::as_str),
        Some("Ctrl+K")
    );
}

#[derive(Default)]
struct TestStorage(std::collections::HashMap<String, String>);

impl eframe::Storage for TestStorage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }

    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value);
    }

    fn flush(&mut self) {}
}

/// Compositor 1.2.1 keeps Auto Select, Show Controls and Snap between runs.
#[test]
fn tool_settings_survive_a_restart() {
    let (_, mut editor) = app();
    editor.auto_select = false;
    editor.show_controls = false;
    editor.snap = true;
    editor.show_rulers = false;
    editor.show_grid = true;
    editor.snap_targets.guides = false;
    editor.snap_targets.grid = false;
    let mut storage = TestStorage::default();
    eframe::App::save(&mut editor, &mut storage);
    let stored = eframe::get_value::<ToolSettings>(&storage, TOOL_SETTINGS_KEY).unwrap();
    assert!(!stored.auto_select);
    assert!(!stored.show_controls);
    assert!(stored.snap);
    assert!(!stored.show_rulers);
    assert!(stored.show_grid);
    assert!(!stored.snap_targets.guides);
    assert!(!stored.snap_targets.grid);
    assert!(stored.snap_targets.canvas);

    // A value written by an older build keeps working and picks up the new defaults.
    let mut legacy = TestStorage::default();
    legacy.0.insert(
        TOOL_SETTINGS_KEY.to_string(),
        "(auto_select:false, show_controls:true, snap:true)".to_string(),
    );
    let stored = eframe::get_value::<ToolSettings>(&legacy, TOOL_SETTINGS_KEY).unwrap();
    assert!(!stored.auto_select);
    assert!(stored.show_controls);
    assert!(stored.snap);
    assert!(stored.show_rulers);
    assert!(!stored.show_grid);
    assert_eq!(stored.snap_targets, SnapTargets::default());
}

#[test]
fn grid_toggle_and_settings_use_document_history() {
    let (context, mut app) = app();
    app.dimensions = [64, 48];
    app.new_document();

    let mut revision = app.session().unwrap().history.revision;
    app.command("toggle_grid");
    assert!(app.show_grid);
    assert_eq!(
        app.session().unwrap().document.grid,
        Some(mectov::document::GridSettings::default())
    );
    assert!(app.session().unwrap().history.revision > revision);

    // Hiding and showing a grid the document already carries is not an edit.
    revision = app.session().unwrap().history.revision;
    app.command("toggle_grid");
    assert!(!app.show_grid);
    app.command("toggle_grid");
    assert!(app.show_grid);
    assert_eq!(app.session().unwrap().history.revision, revision);

    app.command("undo");
    assert!(app.session().unwrap().document.grid.is_none());
    assert!(app.session().unwrap().history.revision < revision);

    app.show_grid = false;
    revision = app.session().unwrap().history.revision;
    app.command("toggle_grid");
    assert!(app.show_grid);
    assert!(
        app.session().unwrap().history.revision > revision,
        "the grid comes back as a new edit"
    );
    revision = app.session().unwrap().history.revision;

    app.command("grid_settings");
    assert!(app.dialog == Some(Dialog::Grid));
    frame(&context, &mut app);
    let cancel = layer_label(&context, &mut app, "Cancel") + Vec2::splat(5.0);
    pointer_frame(&context, &mut app, cancel, None, egui::Modifiers::NONE);
    pointer_frame(
        &context,
        &mut app,
        cancel,
        Some(true),
        egui::Modifiers::NONE,
    );
    pointer_frame(
        &context,
        &mut app,
        cancel,
        Some(false),
        egui::Modifiers::NONE,
    );
    assert!(app.dialog.is_none());
    assert_eq!(app.session().unwrap().history.revision, revision);

    // Applying the dialog writes the edited spacing through the document history.
    app.command("grid_settings");
    frame(&context, &mut app);
    let field = layer_label(&context, &mut app, "32 px") + Vec2::new(16.0, 8.0);
    pointer_frame(&context, &mut app, field, None, egui::Modifiers::NONE);
    let _ = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(1280.0, 860.0),
            )),
            events: vec![
                egui::Event::PointerMoved(field),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: Vec2::new(0.0, 3.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            time: Some(app.frames as f64 / 60.0),
            ..Default::default()
        },
        |ctx| app.show(ctx),
    );
    let apply = layer_label(&context, &mut app, "Apply") + Vec2::splat(5.0);
    pointer_frame(&context, &mut app, apply, None, egui::Modifiers::NONE);
    pointer_frame(&context, &mut app, apply, Some(true), egui::Modifiers::NONE);
    pointer_frame(
        &context,
        &mut app,
        apply,
        Some(false),
        egui::Modifiers::NONE,
    );
    assert!(app.dialog.is_none());
    let grid = app.session().unwrap().document.grid.unwrap();
    assert_ne!(grid.spacing, 32.0, "{grid:?}");
    assert_eq!(grid.subdivisions, 1);
    assert!(app.session().unwrap().history.revision > revision);
    app.command("undo");
    assert_eq!(
        app.session().unwrap().document.grid,
        Some(mectov::document::GridSettings::default())
    );
}

#[test]
fn dragging_a_ruler_creates_one_undoable_guide() {
    let (context, mut app) = app();
    app.dimensions = [64, 48];
    app.new_document();
    frame(&context, &mut app);
    let rects = app.ruler_rects.unwrap();
    let position = Pos2::new(rects[0].center().x, rects[0].center().y);
    pointer_frame(
        &context,
        &mut app,
        position,
        Some(true),
        egui::Modifiers::NONE,
    );
    pointer_frame(
        &context,
        &mut app,
        position + Vec2::new(20.0, 0.0),
        None,
        egui::Modifiers::NONE,
    );
    pointer_frame(
        &context,
        &mut app,
        position + Vec2::new(20.0, 0.0),
        Some(false),
        egui::Modifiers::NONE,
    );
    assert_eq!(app.session().unwrap().document.guides.len(), 1);
    assert_eq!(
        app.session().unwrap().document.guides[0].axis,
        GuideAxis::Vertical
    );
    app.command("undo");
    assert!(app.session().unwrap().document.guides.is_empty());
}

#[test]
fn dragging_an_existing_guide_moves_it_and_undoes() {
    let (context, mut app) = app();
    app.dimensions = [64, 48];
    app.new_document();
    frame(&context, &mut app);
    let rects = app.ruler_rects.unwrap();
    let position = Pos2::new(rects[0].center().x, rects[0].center().y);
    pointer_frame(
        &context,
        &mut app,
        position,
        Some(true),
        egui::Modifiers::NONE,
    );
    pointer_frame(
        &context,
        &mut app,
        position,
        Some(false),
        egui::Modifiers::NONE,
    );
    let created = app.session().unwrap().document.guides[0].position;

    app.command("undo");
    app.tool = Tool::Move;
    app.session_mut()
        .unwrap()
        .document
        .guides
        .push(mectov::document::Guide {
            axis: GuideAxis::Vertical,
            position: created,
        });
    frame(&context, &mut app);
    let canvas = app.canvas_rect.unwrap();
    let zoom = app.session().unwrap().zoom;
    let grab = Pos2::new(canvas.left() + created * zoom, canvas.center().y);
    let dropped = Pos2::new(grab.x + 40.0, grab.y);
    pointer_frame(&context, &mut app, grab, Some(true), egui::Modifiers::NONE);
    pointer_frame(
        &context,
        &mut app,
        dropped,
        Some(false),
        egui::Modifiers::NONE,
    );
    let guides = &app.session().unwrap().document.guides;
    assert_eq!(guides.len(), 1);
    assert!(guides[0].position > created, "{created} -> {:?}", guides[0]);
    assert!(app.session().unwrap().document.selection.is_none());
    app.command("undo");
    assert_eq!(app.session().unwrap().document.guides[0].position, created);
}

#[test]
fn releasing_a_guide_outside_the_canvas_deletes_it() {
    let (context, mut app) = app();
    app.dimensions = [64, 48];
    app.new_document();
    frame(&context, &mut app);
    let rects = app.ruler_rects.unwrap();
    let position = Pos2::new(rects[0].center().x, rects[0].center().y);
    pointer_frame(
        &context,
        &mut app,
        position,
        Some(true),
        egui::Modifiers::NONE,
    );
    pointer_frame(
        &context,
        &mut app,
        position,
        Some(false),
        egui::Modifiers::NONE,
    );
    assert_eq!(app.session().unwrap().document.guides.len(), 1);

    app.tool = Tool::Move;
    frame(&context, &mut app);
    let canvas = app.canvas_rect.unwrap();
    let zoom = app.session().unwrap().zoom;
    let guide = app.session().unwrap().document.guides[0].position;
    let grab = Pos2::new(canvas.left() + guide * zoom, canvas.center().y);
    pointer_frame(&context, &mut app, grab, Some(true), egui::Modifiers::NONE);
    pointer_frame(
        &context,
        &mut app,
        Pos2::new(grab.x, rects[0].center().y),
        Some(false),
        egui::Modifiers::NONE,
    );
    assert!(app.session().unwrap().document.guides.is_empty());
    app.command("undo");
    assert_eq!(app.session().unwrap().document.guides.len(), 1);
}

#[test]
fn moving_a_layer_snaps_to_guides_and_the_grid() {
    let (context, mut app) = app();
    let [bottom, _] = canvas_layers(&mut app);
    app.snap = true;
    app.snap_targets = SnapTargets {
        canvas: true,
        layers: true,
        guides: true,
        grid: true,
    };
    app.session_mut()
        .unwrap()
        .document
        .guides
        .push(mectov::document::Guide {
            axis: GuideAxis::Vertical,
            position: 22.0,
        });
    app.command("toggle_grid");
    app.session_mut().unwrap().document.grid = Some(mectov::document::GridSettings {
        spacing: 25.0,
        subdivisions: 1,
    });
    app.session_mut().unwrap().document.select(bottom, false);
    app.tool = Tool::Move;

    // The left edge lands within a pixel of the guide, so it locks onto the guide
    // rather than the 25 px grid step that also covers it.
    let start = Point::new(15.0, 45.0);
    drag(
        &context,
        &mut app,
        start,
        Point::new(start.x + 11.6, start.y),
        egui::Modifiers::NONE,
    );
    let x = app.session().unwrap().document.layers[0].transform.x;
    assert!((x - 22.0).abs() < 0.01, "{x}");
    app.command("undo");
    assert_eq!(app.session().unwrap().document.layers[0].transform.x, 10.0);

    // With guides out of the picture the grid rounds the same drag on its own.
    app.snap_targets.guides = false;
    app.snap_targets.canvas = false;
    drag(
        &context,
        &mut app,
        start,
        Point::new(start.x + 15.4, start.y),
        egui::Modifiers::NONE,
    );
    let x = app.session().unwrap().document.layers[0].transform.x;
    assert!((x - 25.0).abs() < 0.01, "{x}");
}

#[test]
fn grid_snapping_can_be_disabled_per_target() {
    let (context, mut app) = app();
    let [bottom, _] = canvas_layers(&mut app);
    app.snap = true;
    app.snap_targets = SnapTargets {
        canvas: false,
        layers: false,
        guides: false,
        grid: false,
    };
    app.command("toggle_grid");
    app.session_mut().unwrap().document.grid = Some(mectov::document::GridSettings {
        spacing: 8.0,
        subdivisions: 1,
    });
    app.session_mut().unwrap().document.select(bottom, false);
    app.tool = Tool::Move;

    let start = Point::new(15.0, 45.0);
    drag(
        &context,
        &mut app,
        start,
        Point::new(start.x + 11.6, start.y),
        egui::Modifiers::NONE,
    );
    let x = app.session().unwrap().document.layers[0].transform.x;
    assert!((x - 21.6).abs() < 0.01, "{x}");
    app.command("undo");

    app.snap_targets.grid = true;
    drag(
        &context,
        &mut app,
        start,
        Point::new(start.x + 11.6, start.y),
        egui::Modifiers::NONE,
    );
    let x = app.session().unwrap().document.layers[0].transform.x;
    // The right edge is the closest grid multiple, so the 50 px wide layer
    // settles with its right edge on 72 and its left edge on 22.
    assert!((x + 50.0 - 72.0).abs() < 0.01, "{x}");
}

fn canvas_lines(output: &egui::FullOutput, color: egui::Color32) -> Vec<[Pos2; 2]> {
    output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::LineSegment { points, stroke } if stroke.color == color => Some(*points),
            _ => None,
        })
        .collect()
}

fn ruler_labels(output: &egui::FullOutput, ruler: egui::Rect) -> Vec<String> {
    output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text)
                if ruler.contains(text.pos + Vec2::splat(2.0))
                    && text.galley.text().parse::<f32>().is_ok() =>
            {
                Some(text.galley.text().to_string())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn rulers_grid_guides_and_snap_lines_reach_the_canvas() {
    let (context, mut app) = app();
    let [bottom, _] = canvas_layers(&mut app);
    let grid_color = egui::Color32::from_rgba_unmultiplied(128, 128, 128, 46);
    let guide_color = egui::Color32::from_rgba_unmultiplied(0, 255, 255, 230);
    let snap_layer_color = egui::Color32::from_rgb(219, 115, 213);
    let snap_guide_color = egui::Color32::from_rgb(0, 255, 255);

    // Rulers label both edges, and no grid is drawn while the document has none.
    let output = frame(&context, &mut app);
    let rects = app.ruler_rects.unwrap();
    assert!(
        ruler_labels(&output, rects[0]).len() > 2,
        "{:?}",
        ruler_labels(&output, rects[0])
    );
    assert!(
        ruler_labels(&output, rects[1]).len() > 2,
        "{:?}",
        ruler_labels(&output, rects[1])
    );
    assert!(canvas_lines(&output, grid_color).is_empty());

    app.session_mut().unwrap().document.grid = Some(mectov::document::GridSettings {
        spacing: 20.0,
        subdivisions: 2,
    });
    app.show_grid = true;
    let output = frame(&context, &mut app);
    let lines = canvas_lines(&output, grid_color);
    let vertical = lines
        .iter()
        .filter(|[a, b]| (a.x - b.x).abs() < 0.01)
        .count();
    let horizontal = lines.len() - vertical;
    assert!(vertical > 4 && horizontal > 4, "{lines:?}");

    // Snapping to a guide draws its own line while the layer is being dragged.
    app.session_mut()
        .unwrap()
        .document
        .guides
        .push(mectov::document::Guide {
            axis: GuideAxis::Vertical,
            position: 22.0,
        });
    app.snap = true;
    app.snap_targets = SnapTargets {
        canvas: false,
        layers: false,
        guides: true,
        grid: false,
    };
    app.session_mut().unwrap().document.select(bottom, false);
    app.tool = Tool::Move;
    frame(&context, &mut app);
    let canvas = app.canvas_rect.unwrap();
    let zoom = app.session().unwrap().zoom;
    let from = Pos2::new(canvas.left() + 15.0 * zoom, canvas.top() + 45.0 * zoom);
    let to = Pos2::new(canvas.left() + 26.6 * zoom, canvas.top() + 45.0 * zoom);
    pointer_frame(&context, &mut app, from, Some(true), egui::Modifiers::NONE);
    // The indicator a drag computes is painted on the following frame, the same
    // way the guide follows the pointer.
    let _ = pointer_frame(&context, &mut app, to, None, egui::Modifiers::NONE);
    let output = pointer_frame(&context, &mut app, to, None, egui::Modifiers::NONE);
    let snapped = canvas_lines(&output, snap_guide_color)
        .into_iter()
        .filter(|[a, b]| (a.x - b.x).abs() < 0.01 && b.y - a.y > 100.0)
        .collect::<Vec<_>>();
    assert!(
        !snapped.is_empty(),
        "a snapped guide draws a full-height line"
    );
    assert!(
        canvas_lines(&output, snap_layer_color).is_empty(),
        "layers are not a target here"
    );
    pointer_frame(&context, &mut app, to, Some(false), egui::Modifiers::NONE);
    assert!((app.session().unwrap().document.layers[0].transform.x - 22.0).abs() < 0.01);

    // The stored guide draws over the canvas, and a layer target recolors the line.
    let output = frame(&context, &mut app);
    let guides = canvas_lines(&output, guide_color);
    assert!(!guides.is_empty(), "the guide is drawn across the canvas");
    app.snap_targets = SnapTargets {
        canvas: true,
        layers: true,
        guides: false,
        grid: false,
    };
    // Two tenths short of the top layer's left edge, so the moved layer locks onto it.
    let from = Pos2::new(canvas.left() + 30.0 * zoom, canvas.top() + 45.0 * zoom);
    let to = Pos2::new(canvas.left() + 27.8 * zoom, canvas.top() + 45.0 * zoom);
    pointer_frame(&context, &mut app, from, Some(true), egui::Modifiers::NONE);
    let _ = pointer_frame(&context, &mut app, to, None, egui::Modifiers::NONE);
    let output = pointer_frame(&context, &mut app, to, None, egui::Modifiers::NONE);
    assert!(
        !canvas_lines(&output, snap_layer_color).is_empty(),
        "a layer target draws a magenta line"
    );
    pointer_frame(&context, &mut app, to, Some(false), egui::Modifiers::NONE);
    assert!((app.session().unwrap().document.layers[0].transform.x - 20.0).abs() < 0.01);

    // Hiding the grid drops its lines again.
    app.show_grid = false;
    let output = frame(&context, &mut app);
    assert!(canvas_lines(&output, grid_color).is_empty());
}

fn boxed_text_layer(app: &mut EditorApp, content: &str, width: f32) -> Uuid {
    app.start_text(None, Point::new(20.0, 30.0));
    let edit = app.text_edit.as_mut().unwrap();
    edit.style.content = content.into();
    edit.style.size = 20.0;
    edit.style.r#box = Some(mectov::text::TextBox {
        width,
        min_height: 0.0,
        ..Default::default()
    });
    app.preview_text();
    app.finish_text(true);
    app.session().unwrap().document.active.unwrap()
}

#[test]
fn a_paragraph_box_is_created_edited_and_removed_through_the_dialog() {
    let (context, mut app) = app();
    app.dimensions = [640, 480];
    app.new_document();
    frame(&context, &mut app);
    let id = boxed_text_layer(
        &mut app,
        "A paragraph that needs to wrap inside its box",
        160.0,
    );
    let layer = app.session().unwrap().document.active().unwrap().clone();
    let text_box = layer.text.as_ref().unwrap().r#box.unwrap();
    assert_eq!(text_box.width, 160.0);
    assert_eq!(layer.transform.width, 160.0, "the layer is the box");
    assert!(layer.transform.height > 20.0, "{:?}", layer.transform);
    assert!(app.dialog.is_none());

    // The box controls live in the dialog and preview as they change.
    app.start_text(Some(id), Point::default());
    assert!(app.dialog == Some(Dialog::Text));
    let edit = app.text_edit.as_mut().unwrap();
    assert_eq!(edit.style.r#box.unwrap().width, 160.0);
    edit.style.r#box.as_mut().unwrap().align = mectov::text::TextAlign::Center;
    edit.style.r#box.as_mut().unwrap().line_spacing = 2.0;
    edit.style.r#box.as_mut().unwrap().min_height = 200.0;
    app.preview_text();
    let layer = app.session().unwrap().document.active().unwrap().clone();
    assert_eq!(layer.transform.width, 160.0);
    assert_eq!(layer.transform.height, 200.0, "the box reserves its height");
    app.finish_text(true);

    // Turning the box off returns the layer to a tight text raster.
    app.start_text(Some(id), Point::default());
    app.text_edit.as_mut().unwrap().style.r#box = None;
    app.preview_text();
    app.finish_text(true);
    let layer = app.session().unwrap().document.active().unwrap();
    assert!(layer.text.as_ref().unwrap().r#box.is_none());
    assert!(layer.pixels.as_ref().unwrap().height() < 200);
    assert!(
        layer.pixels.as_ref().unwrap().width() > 160,
        "without a box the raster is the ink, which is wider than the box was"
    );
    assert!(layer.transform.height < 200.0);
}

#[test]
fn a_paragraph_box_rewraps_when_the_header_width_changes_or_a_handle_moves() {
    let (context, mut app) = app();
    app.dimensions = [640, 480];
    app.new_document();
    frame(&context, &mut app);
    let id = boxed_text_layer(
        &mut app,
        "Rewrapping happens whenever the paragraph box width changes",
        150.0,
    );
    let wide = app.session().unwrap().document.active().unwrap().clone();
    let wide_height = wide.pixels.as_ref().unwrap().height();

    // The tool header re-flows the box through one undoable edit.
    app.tool = Tool::Text;
    frame(&context, &mut app);
    let width = field_right_of(&context, &mut app, "Box width");
    for _ in 0..2 {
        let _ = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    Vec2::new(1280.0, 860.0),
                )),
                events: vec![
                    egui::Event::PointerMoved(width),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: Vec2::new(0.0, -25.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                time: Some(app.frames as f64 / 60.0),
                ..Default::default()
            },
            |ctx| app.show(ctx),
        );
    }
    frame(&context, &mut app);
    let narrow = app.session().unwrap().document.active().unwrap().clone();
    let text_box = narrow.text.as_ref().unwrap().r#box.unwrap();
    assert_eq!(text_box.width, 100.0, "the header number drives the box");
    assert_eq!(narrow.transform.width, text_box.width);
    assert_eq!(narrow.pixels.as_ref().unwrap().width(), 100);
    assert!(
        narrow.pixels.as_ref().unwrap().height() > wide_height,
        "a narrower box wraps onto more lines"
    );
    app.session_mut().unwrap().history.commit();

    // Dragging a side handle with the Move tool re-flows the box as it moves.
    app.tool = Tool::Move;
    app.show_controls = true;
    frame(&context, &mut app);
    let before = app.session().unwrap().document.active().unwrap().clone();
    let handle = Point::new(
        before.transform.x + before.transform.width,
        before.transform.y + before.transform.height * 0.5,
    );
    drag(
        &context,
        &mut app,
        handle,
        Point::new(handle.x - 40.0, handle.y),
        egui::Modifiers::NONE,
    );
    let after = app.session().unwrap().document.active().unwrap().clone();
    assert!(
        after.transform.width < before.transform.width,
        "{:?} -> {:?}",
        before.transform,
        after.transform
    );
    assert_eq!(
        after.pixels.as_ref().unwrap().width() as f32,
        after.transform.width,
        "the box re-wrapped instead of stretching"
    );
    assert_eq!(
        after.text.as_ref().unwrap().r#box.unwrap().width,
        after.transform.width
    );
    assert!(after.pixels.as_ref().unwrap().height() > wide_height);
    assert_eq!(app.session().unwrap().document.active.unwrap(), id);
    app.command("undo");
    let restored = app.session().unwrap().document.active().unwrap();
    assert_eq!(restored.transform.width, before.transform.width);
    assert_eq!(
        restored.pixels.as_ref().unwrap().width() as f32,
        before.transform.width
    );
}

#[test]
fn a_paragraph_box_outline_is_drawn_for_the_text_tool() {
    let (context, mut app) = app();
    app.dimensions = [640, 480];
    app.new_document();
    frame(&context, &mut app);
    boxed_text_layer(&mut app, "Outline me", 200.0);
    let accent = theme::ACCENT;
    let count = |app: &mut EditorApp| {
        frame(&context, app)
            .shapes
            .iter()
            .filter(|shape| {
                matches!(&shape.shape, egui::Shape::LineSegment { stroke, .. } if stroke.color == accent)
            })
            .count()
    };
    app.tool = Tool::Move;
    let without = count(&mut app);
    app.tool = Tool::Text;
    let with = count(&mut app);
    assert!(with > without, "{with} vs {without}");
}

/// The centre of the first painted box to the right of a label on the same row.
/// The middle of the slider track drawn to the right of a label. A track is the
/// widest thing on its own line, which is what tells it from the canvas
/// backdrop behind the dialog and from the field's own frame.
fn track_right_of(context: &egui::Context, app: &mut EditorApp, label: &str) -> Pos2 {
    let label = layer_label(context, app, label);
    frame(context, app)
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect)
                if rect.rect.left() > label.x && (rect.rect.center().y - label.y).abs() < 12.0 =>
            {
                Some(rect.rect)
            }
            _ => None,
        })
        .max_by(|a, b| a.width().total_cmp(&b.width()))
        .map_or_else(
            || panic!("Missing slider track next to {label}"),
            |rect| egui::pos2(rect.left() + 12.0, rect.center().y),
        )
}

fn field_right_of(context: &egui::Context, app: &mut EditorApp, label: &str) -> Pos2 {
    let label = layer_label(context, app, label);
    frame(context, app)
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect)
                if rect.rect.left() > label.x && (rect.rect.center().y - label.y).abs() < 12.0 =>
            {
                Some(rect.rect)
            }
            _ => None,
        })
        .min_by(|a, b| a.left().total_cmp(&b.left()))
        .map_or_else(
            || panic!("Missing field next to {label}"),
            |rect| rect.center(),
        )
}
