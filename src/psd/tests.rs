use super::*;

fn push_u8(bytes: &mut Vec<u8>, value: u8) {
    bytes.push(value);
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_i16(bytes: &mut Vec<u8>, value: i16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_i32(bytes: &mut Vec<u8>, value: i32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_length(bytes: &mut Vec<u8>, value: usize, variant: Variant) {
    if variant == Variant::Psb {
        push_u64(bytes, value as u64);
    } else {
        push_u32(bytes, value as u32);
    }
}

fn pad_to_four(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

fn raw_channel(values: &[u8]) -> Vec<u8> {
    let mut result = push_compression(0);
    result.extend_from_slice(values);
    result
}

fn push_compression(value: u16) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

fn packbits_row(row: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    let mut position = 0;
    while position < row.len() {
        let mut run = 1;
        while position + run < row.len() && row[position + run] == row[position] {
            run += 1;
        }
        if run >= 3 {
            let count = run.min(128);
            result.push((257 - count) as u8);
            result.push(row[position]);
            position += count;
        } else {
            let start = position;
            let mut count = 1;
            while position + count < row.len()
                && count < 128
                && !(position + count + 2 < row.len()
                    && row[position + count] == row[position + count + 1]
                    && row[position + count] == row[position + count + 2])
            {
                count += 1;
            }
            result.push((count - 1) as u8);
            result.extend_from_slice(&row[start..start + count]);
            position += count;
        }
    }
    result
}

fn rle_channel(values: &[u8], width: usize, height: usize, variant: Variant) -> Vec<u8> {
    let mut rows = Vec::new();
    for y in 0..height {
        rows.push(packbits_row(&values[y * width..(y + 1) * width]));
    }
    let mut result = push_compression(1);
    let table_size = if variant == Variant::Psd { 2 } else { 4 };
    for row in &rows {
        if table_size == 2 {
            push_u16(&mut result, row.len() as u16);
        } else {
            push_u32(&mut result, row.len() as u32);
        }
    }
    for row in rows {
        result.extend(row);
    }
    result
}

#[derive(Clone)]
struct TestChannel {
    id: i16,
    data: Vec<u8>,
}

#[derive(Clone)]
struct TestMask {
    rect: [i32; 4],
    background: u8,
    flags: u8,
    data: Vec<u8>,
}

#[derive(Clone)]
struct TestLayer {
    rect: [i32; 4],
    channels: Vec<TestChannel>,
    name: String,
    blend: [u8; 4],
    opacity: u8,
    clipping: u8,
    flags: u8,
    mask: Option<TestMask>,
    divider: Option<u32>,
    fill_opacity: Option<u8>,
    invert: bool,
    /// Extra tagged blocks, so a test can attach records the parser must read.
    tagged: Vec<([u8; 4], Vec<u8>)>,
}

impl TestLayer {
    fn raster(name: &str, values: [Vec<u8>; 4], width: usize, height: usize) -> Self {
        Self {
            rect: [0, 0, height as i32, width as i32],
            channels: [0i16, 1, 2, -1]
                .into_iter()
                .enumerate()
                .map(|(index, id)| TestChannel {
                    id,
                    data: raw_channel(&values[index]),
                })
                .collect(),
            name: name.to_owned(),
            blend: *b"norm",
            opacity: 255,
            clipping: 0,
            flags: 0,
            mask: None,
            divider: None,
            fill_opacity: None,
            invert: false,
            tagged: Vec::new(),
        }
    }
}

fn tagged_block(key: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    result.extend_from_slice(b"8BIM");
    result.extend_from_slice(key);
    push_u32(&mut result, data.len() as u32);
    result.extend_from_slice(data);
    result
}

fn layer_record(layer: &TestLayer, variant: Variant) -> Vec<u8> {
    let mut extra = Vec::new();
    if let Some(mask) = &layer.mask {
        let mut block = Vec::new();
        for value in mask.rect {
            push_i32(&mut block, value);
        }
        push_u8(&mut block, mask.background);
        push_u8(&mut block, mask.flags);
        pad_to_four(&mut block);
        push_u32(&mut extra, block.len() as u32);
        extra.extend(block);
    } else {
        push_u32(&mut extra, 0);
    }
    push_u32(&mut extra, 0);
    push_u8(&mut extra, layer.name.len() as u8);
    extra.extend_from_slice(layer.name.as_bytes());
    pad_to_four(&mut extra);
    if let Some(divider) = layer.divider {
        let mut data = divider.to_be_bytes().to_vec();
        data.extend_from_slice(b"8BIMpass");
        extra.extend(tagged_block(b"lsct", &data));
    }
    if let Some(fill) = layer.fill_opacity {
        extra.extend(tagged_block(b"iOpa", &[fill]));
    }
    if layer.invert {
        extra.extend(tagged_block(b"nvrt", &[]));
    }
    for (key, data) in &layer.tagged {
        extra.extend(tagged_block(key, data));
    }
    let mut record = Vec::new();
    for value in layer.rect {
        push_i32(&mut record, value);
    }
    push_u16(&mut record, layer.channels.len() as u16);
    for channel in &layer.channels {
        push_i16(&mut record, channel.id);
        if variant == Variant::Psb {
            push_u64(&mut record, channel.data.len() as u64);
        } else {
            push_u32(&mut record, channel.data.len() as u32);
        }
    }
    record.extend_from_slice(b"8BIM");
    record.extend_from_slice(&layer.blend);
    push_u8(&mut record, layer.opacity);
    push_u8(&mut record, layer.clipping);
    push_u8(&mut record, layer.flags);
    push_u8(&mut record, 0);
    push_u32(&mut record, extra.len() as u32);
    record.extend(extra);
    record
}

fn build_psd(width: u32, height: u32, variant: Variant, layers: &[TestLayer]) -> Vec<u8> {
    let mut records = Vec::new();
    let mut channel_data = Vec::new();
    for layer in layers {
        records.extend(layer_record(layer, variant));
        for channel in &layer.channels {
            channel_data.extend_from_slice(&channel.data);
        }
    }
    let mut layer_info = Vec::new();
    push_i16(&mut layer_info, layers.len() as i16);
    layer_info.extend(records);
    layer_info.extend(channel_data);
    let mut layer_mask = Vec::new();
    push_length(&mut layer_mask, layer_info.len(), variant);
    layer_mask.extend(layer_info);
    push_u32(&mut layer_mask, 0);
    let mut result = Vec::new();
    result.extend_from_slice(b"8BPS");
    push_u16(&mut result, if variant == Variant::Psd { 1 } else { 2 });
    result.extend_from_slice(&[0; 6]);
    push_u16(&mut result, 4);
    push_u32(&mut result, height);
    push_u32(&mut result, width);
    push_u16(&mut result, 8);
    push_u16(&mut result, 3);
    push_u32(&mut result, 0);
    push_u32(&mut result, 0);
    push_length(&mut result, layer_mask.len(), variant);
    result.extend(layer_mask);
    push_u16(&mut result, 0);
    let plane = vec![255; width as usize * height as usize];
    for _ in 0..4 {
        result.extend(plane.iter());
    }
    result
}

fn sample_layer() -> TestLayer {
    TestLayer::raster(
        "Background",
        [
            vec![255, 0, 0, 255],
            vec![0, 255, 0, 255],
            vec![0, 0, 255, 255],
            vec![10, 20, 30, 40],
        ],
        2,
        2,
    )
}

#[test]
fn imports_raw_and_rle_psd_channels() {
    let layer = sample_layer();
    let document = parse(&build_psd(2, 2, Variant::Psd, &[layer])).unwrap();
    assert_eq!(document.width, 2);
    assert_eq!(document.layers.len(), 1);
    assert_eq!(
        document.layers[0]
            .pixels
            .as_ref()
            .unwrap()
            .get_pixel(0, 0)
            .0,
        [255, 0, 0, 10]
    );

    let mut layer = sample_layer();
    for channel in &mut layer.channels {
        let values = match channel.id {
            -1 => vec![10, 20, 30, 40],
            0 => vec![255, 0, 255, 0],
            1 => vec![0, 255, 0, 255],
            _ => vec![0, 0, 255, 255],
        };
        channel.data = rle_channel(&values, 2, 2, Variant::Psd);
    }
    let document = parse(&build_psd(2, 2, Variant::Psd, &[layer])).unwrap();
    assert_eq!(
        document.layers[0]
            .pixels
            .as_ref()
            .unwrap()
            .get_pixel(1, 0)
            .0,
        [0, 255, 0, 20]
    );
}

#[test]
fn imports_psb_wide_lengths() {
    let layer = sample_layer();
    let document = parse(&build_psd(2, 2, Variant::Psb, &[layer])).unwrap();
    assert_eq!(
        document.layers[0].pixels.as_ref().unwrap().dimensions(),
        (2, 2)
    );
}

#[test]
fn imports_masks_and_clipping() {
    let mut base = sample_layer();
    base.name = "Base".into();
    let mut clipped = sample_layer();
    clipped.name = "Clipped".into();
    clipped.clipping = 1;
    clipped.rect = [0, 0, 2, 2];
    let mask_values = vec![255, 128, 64, 0];
    clipped.mask = Some(TestMask {
        rect: [0, 0, 2, 2],
        background: 0,
        flags: 0,
        data: raw_channel(&mask_values),
    });
    let mask_channel = clipped.mask.as_ref().unwrap().data.clone();
    clipped.channels.push(TestChannel {
        id: -2,
        data: mask_channel,
    });
    let document = parse(&build_psd(2, 2, Variant::Psd, &[base, clipped])).unwrap();
    assert_eq!(document.layers.len(), 2);
    assert_eq!(document.layers[1].clip_to, Some(document.layers[0].id));
    let mask = document.layers[1].mask.as_ref().unwrap();
    assert_eq!(mask.pixels.get_pixel(1, 0)[0], 128);
}

#[test]
fn imports_pass_through_groups() {
    let marker_channels: Vec<_> = [0i16, 1, 2, -1]
        .into_iter()
        .map(|id| TestChannel {
            id,
            data: push_compression(0),
        })
        .collect();
    let boundary = TestLayer {
        rect: [0, 0, 0, 0],
        channels: marker_channels,
        name: "Group".into(),
        blend: *b"norm",
        opacity: 255,
        clipping: 0,
        flags: 0,
        mask: None,
        divider: Some(3),
        fill_opacity: None,
        invert: false,
        tagged: Vec::new(),
    };
    let child = sample_layer();
    let mut closing = boundary.clone();
    closing.divider = Some(2);
    closing.name = "Folder".into();
    let document = parse(&build_psd(2, 2, Variant::Psd, &[boundary, child, closing])).unwrap();
    assert_eq!(document.layers.len(), 2);
    let group = document.layers.iter().find(|layer| layer.group).unwrap();
    let child = document.layers.iter().find(|layer| !layer.group).unwrap();
    assert_eq!(child.parent, Some(group.id));
    assert_eq!(group.name, "Folder");
}

#[test]
fn imports_flattened_composite_invert_and_blend_modes() {
    let default_ranges = [0u8, 0, 255, 255].repeat(10);
    let non_default_ranges = [0u8, 0, 255, 254].repeat(10);
    assert!(is_default_blending_ranges(&default_ranges));
    assert!(!is_default_blending_ranges(&non_default_ranges));
    let flattened = parse(&build_psd(2, 2, Variant::Psd, &[])).unwrap();
    assert_eq!(flattened.layers.len(), 1);
    assert_eq!(
        flattened.layers[0]
            .pixels
            .as_ref()
            .unwrap()
            .get_pixel(0, 0)
            .0,
        [255, 255, 255, 255]
    );

    let invert = TestLayer {
        rect: [0, 0, 0, 0],
        channels: Vec::new(),
        name: "Invert".into(),
        blend: *b"norm",
        opacity: 255,
        clipping: 0,
        flags: 0,
        mask: None,
        divider: None,
        fill_opacity: None,
        invert: true,
        tagged: Vec::new(),
    };
    let document = parse(&build_psd(2, 2, Variant::Psd, &[invert])).unwrap();
    assert_eq!(document.layers[0].adjustment, Some(Adjustment::Invert));

    let mut layer = sample_layer();
    layer.blend = *b"mul ";
    layer.opacity = 128;
    let document = parse(&build_psd(2, 2, Variant::Psd, &[layer])).unwrap();
    assert_eq!(document.layers[0].blend, BlendMode::Multiply);
    assert_eq!(document.layers[0].opacity, 128.0 / 255.0);

    let mut unsupported = sample_layer();
    unsupported.blend = *b"diss";
    assert!(parse(&build_psd(2, 2, Variant::Psd, &[unsupported])).is_err());
}

#[test]
fn rejects_truncated_and_unsupported_files() {
    let mut bytes = build_psd(2, 2, Variant::Psd, &[sample_layer()]);
    bytes.truncate(20);
    assert!(parse(&bytes).is_err());
    let mut layer = sample_layer();
    layer.channels[0].data = push_compression(2);
    assert!(parse(&build_psd(2, 2, Variant::Psd, &[layer])).is_err());
}

/// A Photoshop type layer's `TySh` block: the text's own transform, two version
/// numbers, and the descriptor that points at the engine data.
struct TypeLayer {
    content: String,
    family: String,
    size: f64,
    color: [u8; 3],
    align: &'static str,
    scale: f64,
    engines: bool,
    /// Version 1 writes the transform as eight 16-bit halves, version 2 as six
    /// doubles, so a test can ask for either shape.
    version: u32,
}

impl TypeLayer {
    fn new(content: &str) -> Self {
        Self {
            content: content.into(),
            family: "Helvetica".into(),
            size: 24.0,
            color: [255, 0, 0],
            align: "leftJustifyNoLast",
            scale: 1.0,
            engines: true,
            version: 2,
        }
    }

    fn block(&self) -> Vec<u8> {
        let mut data = Vec::new();
        push_u32(&mut data, self.version);
        if self.version == 1 {
            // Four 16.16 fixed-point numbers: xx, xy, yx, yy.
            for value in [self.scale, 0.0, 0.0, self.scale] {
                let fixed = (value * 65536.0) as i32;
                push_u16(&mut data, (fixed >> 16) as u16);
                push_u16(&mut data, fixed as u16);
            }
        } else {
            for value in [self.scale, 0.0, 0.0, self.scale, 0.0, 0.0] {
                push_u64(&mut data, value.to_bits());
            }
        }
        push_u32(&mut data, 50);
        push_u32(&mut data, 16);
        data.extend(self.descriptor());
        // The warp and the bounding box follow; this port reads none of them.
        push_u32(&mut data, 0);
        for value in [0.0f64, 200.0, 0.0, 60.0] {
            push_u64(&mut data, value.to_bits());
        }
        data
    }

    fn descriptor(&self) -> Vec<u8> {
        let mut items = Vec::new();
        if self.engines {
            items.push(item_bytes(b"EngineData", b"Obj ", &self.engine_data()));
        }
        // The justification lives in the paragraph runs, not on the descriptor.
        let paragraph = descriptor_bytes(
            b"ParagraphRun",
            &[item_bytes(
                b"Justification",
                b"enum",
                &[
                    descriptor_key(b"Ordn"),
                    descriptor_key(self.align.as_bytes()),
                    descriptor_key(b"Justification"),
                ]
                .concat(),
            )],
        );
        items.push(item_bytes(b"ParagraphRun", b"Obj ", &paragraph));
        descriptor_bytes(b"textLayer", &items)
    }

    fn engine_data(&self) -> Vec<u8> {
        let fonts = descriptor_bytes(
            b"Font",
            &[
                item_bytes(b"Name", b"TEXT", &utf16(&self.family)),
                item_bytes(b"Sz  ", b"dbl ", &self.size.to_bits().to_be_bytes()),
            ],
        );
        let font_set = descriptor_bytes(
            b"FontSet",
            &[item_bytes(b"Font", b"VlLs", &list_bytes(&[fonts]))],
        );
        let resources = descriptor_bytes(
            b"DocumentResources",
            &[item_bytes(
                b"ResourceDict",
                b"Obj ",
                &descriptor_bytes(
                    b"ResourceDict",
                    &[item_bytes(b"FontSet", b"Obj ", &font_set)],
                ),
            )],
        );
        let colour = descriptor_bytes(
            b"RGBColor",
            &[
                item_bytes(b"Rd  ", b"long", &i32::from(self.color[0]).to_be_bytes()),
                item_bytes(b"Grn ", b"long", &i32::from(self.color[1]).to_be_bytes()),
                item_bytes(b"Bl  ", b"long", &i32::from(self.color[2]).to_be_bytes()),
            ],
        );
        let style = descriptor_bytes(
            b"FontStyle",
            &[
                item_bytes(b"Font", b"long", &0i32.to_be_bytes()),
                item_bytes(b"FillColor", b"Obj ", &colour),
            ],
        );
        let run = descriptor_bytes(
            b"TextRun",
            &[
                item_bytes(b"RunLength", b"long", &1i32.to_be_bytes()),
                item_bytes(b"Style", b"Obj ", &style),
            ],
        );
        let run_array = descriptor_bytes(
            b"RunArrayCore",
            &[
                item_bytes(
                    b"RunLengthArray",
                    b"VlLs",
                    &typed_list_bytes(&[long_bytes(1)]),
                ),
                item_bytes(
                    b"StyleRunArray",
                    b"Obj ",
                    &descriptor_bytes(
                        b"RunArrayCore",
                        &[item_bytes(b"RunArray", b"VlLs", &list_bytes(&[run]))],
                    ),
                ),
                item_bytes(
                    b"RunTextArray",
                    b"Obj ",
                    &descriptor_bytes(
                        b"RunArrayCore",
                        &[item_bytes(
                            b"RunArray",
                            b"VlLs",
                            &list_bytes(&[descriptor_bytes(
                                b"TextRun",
                                &[item_bytes(b"RunText", b"TEXT", &utf16(&self.content))],
                            )]),
                        )],
                    ),
                ),
            ],
        );
        descriptor_bytes(
            b"EngineDataCore",
            &[
                item_bytes(b"DocumentResources", b"Obj ", &resources),
                item_bytes(b"RunArray", b"Obj ", &run_array),
            ],
        )
    }
}

fn descriptor_key(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    push_u32(&mut out, name.len() as u32);
    out.extend_from_slice(name);
    out
}

fn descriptor_bytes(class: &[u8], items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = descriptor_key(class);
    push_u32(&mut out, items.len() as u32);
    for item in items {
        out.extend_from_slice(item);
    }
    out
}

fn item_bytes(key: &[u8], os_type: &[u8], value: &[u8]) -> Vec<u8> {
    let mut out = descriptor_key(key);
    out.extend_from_slice(os_type);
    out.extend_from_slice(value);
    out
}

fn list_bytes(dicts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    push_u32(&mut out, dicts.len() as u32);
    for dict in dicts {
        out.extend_from_slice(b"Obj ");
        out.extend_from_slice(dict);
    }
    out
}

fn typed_list_bytes(values: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    push_u32(&mut out, values.len() as u32);
    for value in values {
        out.extend_from_slice(value);
    }
    out
}

fn long_bytes(value: i32) -> Vec<u8> {
    let mut out = b"long".to_vec();
    out.extend(value.to_be_bytes());
    out
}

fn utf16(text: &str) -> Vec<u8> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = Vec::new();
    push_u32(&mut out, units.len() as u32);
    for unit in units {
        out.extend(unit.to_be_bytes());
    }
    out
}

#[test]
fn imports_a_type_layer_as_editable_text() {
    let mut type_layer = TypeLayer::new("Hello Photoshop");
    type_layer.align = "centerJustify";
    type_layer.size = 18.0;
    type_layer.scale = 2.0;
    let mut layer = TestLayer::raster(
        "Title",
        [vec![9; 6], vec![9; 6], vec![9; 6], vec![255; 6]],
        3,
        2,
    );
    layer.tagged.push((*b"TySh", type_layer.block()));
    let document = parse(&build_psd(3, 2, Variant::Psd, &[layer])).unwrap();
    let text = document.layers[0].text.as_ref().expect("type layer text");
    assert_eq!(text.content, "Hello Photoshop");
    assert_eq!(text.family, "Helvetica");
    // Photoshop stores points; the block's own scale is folded into the size.
    assert_eq!(text.size, 36.0);
    assert_eq!(text.color, [255, 0, 0, 255]);
    let text_box = text.r#box.expect("a box carries the justification");
    assert_eq!(text_box.align, crate::text::TextAlign::Center);
    assert_eq!(text_box.width, 3.0, "the box is the width Photoshop drew");
    assert_eq!(text_box.min_height, 2.0);
    // Photoshop's own pixels stay, so the layer looks exactly as it did.
    assert_eq!(
        document.layers[0].pixels.as_ref().unwrap().dimensions(),
        (3, 2)
    );
    assert_eq!(document.layers[0].name, "Title");
    document.validate().unwrap();
}

#[test]
fn an_imported_type_layer_survives_a_project_round_trip() {
    let mut type_layer = TypeLayer::new("Round trip");
    type_layer.align = "rightJustify";
    let mut layer = TestLayer::raster(
        "Title",
        [vec![2; 4], vec![2; 4], vec![2; 4], vec![255; 4]],
        2,
        2,
    );
    layer.tagged.push((*b"TySh", type_layer.block()));
    let document = parse(&build_psd(2, 2, Variant::Psd, &[layer])).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("type.mectov");
    crate::io::save(&document, &path).unwrap();
    let loaded = crate::io::load(&path).unwrap();
    let text = loaded.layers[0].text.as_ref().expect("the text survived");
    assert_eq!(text.content, "Round trip");
    assert_eq!(text.family, "Helvetica");
    let text_box = text.r#box.as_ref().expect("the box survived");
    assert_eq!(text_box.align, crate::text::TextAlign::Right);
    // Import notes describe the file that was read, so a project never keeps them.
    assert!(loaded.import_notes.is_empty());
    loaded.validate().unwrap();
}

#[test]
fn a_type_layer_without_readable_engine_data_stays_pixels() {
    let mut type_layer = TypeLayer::new("Lost");
    type_layer.engines = false;
    let mut layer = TestLayer::raster(
        "Title",
        [vec![7; 4], vec![7; 4], vec![7; 4], vec![255; 4]],
        2,
        2,
    );
    layer.tagged.push((*b"TySh", type_layer.block()));
    let document = parse(&build_psd(2, 2, Variant::Psd, &[layer])).unwrap();
    assert!(document.layers[0].text.is_none());
    assert_eq!(
        document.layers[0].pixels.as_ref().unwrap().dimensions(),
        (2, 2)
    );
    document.validate().unwrap();
}

#[test]
fn a_truncated_type_layer_does_not_fail_the_import() {
    let block = TypeLayer::new("Cut short").block();
    // Every truncation must still import as pixels, and the first one that
    // carries text is the first that holds the whole descriptor. The warp and
    // the bounding box follow the descriptor, and this port reads neither, so
    // the last 36 bytes of the block are not needed for text.
    let mut first_with_text = None;
    for length in 4..=block.len() {
        let mut layer = TestLayer::raster(
            "Title",
            [vec![5; 4], vec![5; 4], vec![5; 4], vec![255; 4]],
            2,
            2,
        );
        layer.tagged.push((*b"TySh", block[..length].to_vec()));
        let document = parse(&build_psd(2, 2, Variant::Psd, &[layer])).unwrap();
        assert_eq!(
            document.layers[0].pixels.as_ref().unwrap().dimensions(),
            (2, 2),
            "{length} bytes must still import pixels"
        );
        if document.layers[0].text.is_some() && first_with_text.is_none() {
            first_with_text = Some(length);
        }
    }
    assert_eq!(first_with_text, Some(block.len() - 36));
}

#[test]
fn a_type_layer_in_a_psb_imports_too() {
    let mut type_layer = TypeLayer::new("Wide format");
    type_layer.family = "Georgia".into();
    let mut layer = TestLayer::raster(
        "Title",
        [vec![3; 4], vec![3; 4], vec![3; 4], vec![255; 4]],
        2,
        2,
    );
    layer.tagged.push((*b"TySh", type_layer.block()));
    let document = parse(&build_psd(2, 2, Variant::Psb, &[layer])).unwrap();
    let text = document.layers[0]
        .text
        .as_ref()
        .expect("PSB type layer text");
    assert_eq!(text.content, "Wide format");
    assert_eq!(text.family, "Georgia");
}

#[test]
fn a_layer_with_only_txt2_imports_as_pixels() {
    let mut layer = TestLayer::raster(
        "Title",
        [vec![4; 4], vec![4; 4], vec![4; 4], vec![255; 4]],
        2,
        2,
    );
    // `Txt2` is Adobe's own copy of the text, and this port reads `TySh`.
    layer
        .tagged
        .push((*b"Txt2", b"\x00\x00\x00\x08Adobe".to_vec()));
    let document = parse(&build_psd(2, 2, Variant::Psd, &[layer])).unwrap();
    assert!(document.layers[0].text.is_none());
    assert_eq!(
        document.layers[0].pixels.as_ref().unwrap().dimensions(),
        (2, 2)
    );
    assert!(document.import_notes.is_empty());
}

#[test]
fn a_type_layer_that_cannot_be_read_is_reported_and_kept() {
    let mut type_layer = TypeLayer::new("Broken");
    type_layer.engines = false;
    let mut layer = TestLayer::raster(
        "Title",
        [vec![6; 4], vec![6; 4], vec![6; 4], vec![255; 4]],
        2,
        2,
    );
    layer.tagged.push((*b"TySh", type_layer.block()));
    let document = parse(&build_psd(2, 2, Variant::Psd, &[layer])).unwrap();
    assert!(document.layers[0].text.is_none());
    assert_eq!(
        document.layers[0].pixels.as_ref().unwrap().dimensions(),
        (2, 2)
    );
    assert_eq!(document.import_notes.len(), 1);
    assert!(
        document.import_notes[0].starts_with("Title: "),
        "the note names the layer: {:?}",
        document.import_notes
    );
}

#[test]
fn a_version_one_type_layer_is_read_too() {
    let mut type_layer = TypeLayer::new("Old shape");
    type_layer.version = 1;
    type_layer.size = 20.0;
    type_layer.scale = 1.5;
    let mut layer = TestLayer::raster(
        "Title",
        [vec![8; 4], vec![8; 4], vec![8; 4], vec![255; 4]],
        2,
        2,
    );
    layer.tagged.push((*b"TySh", type_layer.block()));
    let document = parse(&build_psd(2, 2, Variant::Psd, &[layer])).unwrap();
    let text = document.layers[0]
        .text
        .as_ref()
        .expect("version 1 type layer text");
    assert_eq!(text.content, "Old shape");
    // 20 points at the transform's scale of 1.5.
    assert_eq!(text.size, 30.0);
}

#[test]
fn a_transform_scale_this_port_cannot_use_is_left_alone() {
    // A transform that claims no usable scale keeps the run's own size.
    let mut type_layer = TypeLayer::new("Unscaled");
    type_layer.size = 16.0;
    let mut layer = TestLayer::raster(
        "Title",
        [vec![11; 4], vec![11; 4], vec![11; 4], vec![255; 4]],
        2,
        2,
    );
    layer.tagged.push((*b"TySh", type_layer.block()));
    let document = parse(&build_psd(2, 2, Variant::Psd, &[layer])).unwrap();
    assert_eq!(
        document.layers[0].text.as_ref().unwrap().size,
        16.0,
        "a scale of one leaves the size alone"
    );
}
