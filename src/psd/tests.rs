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
