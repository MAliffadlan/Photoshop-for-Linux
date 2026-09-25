use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use image::{GrayImage, Luma, Rgba, RgbaImage};
use uuid::Uuid;

use crate::{
    blend::BlendMode,
    document::{
        Adjustment, Document, Layer, MAX_LAYERS, MAX_PIXELS, Mask, Transform, validate_size,
    },
};

const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_DECODE_BYTES: u64 = 768 * 1024 * 1024;
const MAX_CHANNELS: usize = 56;
const MAX_GROUPS: usize = 64;
const MAX_NAME_BYTES: usize = 16_384;
const MAX_EXTRA_BYTES: u64 = 64 * 1024 * 1024;

fn is_wide_tagged_key(key: &[u8; 4]) -> bool {
    matches!(
        key,
        b"LMsk"
            | b"Lr16"
            | b"Lr32"
            | b"Layr"
            | b"Mtrn"
            | b"Mt16"
            | b"Mt32"
            | b"Alph"
            | b"FMsk"
            | b"lnk2"
            | b"lnk3"
            | b"lnkE"
            | b"FXid"
            | b"FEid"
            | b"FELS"
            | b"PxSD"
            | b"pths"
            | b"extd"
            | b"cinf"
            | b"artd"
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Variant {
    Psd,
    Psb,
}

impl Variant {
    fn wide_lengths(self) -> bool {
        self == Self::Psb
    }
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    top: i32,
    left: i32,
    bottom: i32,
    right: i32,
}

impl Rect {
    fn dimensions(self) -> Result<(u32, u32)> {
        let width = i64::from(self.right) - i64::from(self.left);
        let height = i64::from(self.bottom) - i64::from(self.top);
        ensure!(width >= 0 && height >= 0, "Invalid PSD rectangle");
        ensure!(
            (width == 0) == (height == 0),
            "PSD rectangle has one empty dimension"
        );
        if width == 0 {
            return Ok((0, 0));
        }
        let width = u32::try_from(width)?;
        let height = u32::try_from(height)?;
        validate_size(width, height)?;
        Ok((width, height))
    }
}

struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.position)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .context("PSD offset overflow")?;
        ensure!(end <= self.data.len(), "Unexpected end of PSD data");
        let result = &self.data[self.position..end];
        self.position = end;
        Ok(result)
    }

    fn advance(&mut self, length: usize) -> Result<()> {
        self.take(length).map(|_| ())
    }

    fn read_block(&mut self, length: u64) -> Result<Reader<'a>> {
        let length = usize::try_from(length).context("PSD block is too large")?;
        Ok(Reader::new(self.take(length)?))
    }

    fn read_u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_i16(&mut self) -> Result<i16> {
        let bytes = self.take(2)?;
        Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_i32(&mut self) -> Result<i32> {
        let bytes = self.take(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64> {
        let bytes = self.take(8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_length(&mut self, wide: bool) -> Result<u64> {
        if wide {
            self.read_u64()
        } else {
            Ok(u64::from(self.read_u32()?))
        }
    }
}

struct Header {
    variant: Variant,
    channels: u16,
    width: u32,
    height: u32,
}

fn read_header(reader: &mut Reader<'_>) -> Result<Header> {
    ensure!(reader.take(4)? == b"8BPS", "Not a PSD or PSB file");
    let version = reader.read_u16()?;
    let variant = match version {
        1 => Variant::Psd,
        2 => Variant::Psb,
        _ => bail!("Unsupported PSD version {version}"),
    };
    ensure!(
        reader.take(6)?.iter().all(|byte| *byte == 0),
        "Invalid PSD reserved header bytes"
    );
    let channels = reader.read_u16()?;
    ensure!(
        (3..=4).contains(&channels),
        "PSD import supports RGB documents with three or four channels"
    );
    let height = reader.read_u32()?;
    let width = reader.read_u32()?;
    validate_size(width, height)?;
    ensure!(
        reader.read_u16()? == 8,
        "PSD import supports 8-bit channels only"
    );
    ensure!(
        reader.read_u16()? == 3,
        "PSD import supports RGB color mode only"
    );
    Ok(Header {
        variant,
        channels,
        width,
        height,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Divider {
    Open,
    Closed,
    Boundary,
}

#[derive(Clone, Debug)]
struct ChannelInfo {
    id: i16,
    length: u64,
}

#[derive(Clone, Debug)]
struct RawMask {
    rect: Rect,
    background: u8,
    flags: u8,
}

#[derive(Clone, Debug)]
struct RawLayer {
    rect: Rect,
    channels: Vec<ChannelInfo>,
    name: String,
    blend_key: [u8; 4],
    opacity: u8,
    clipping: u8,
    flags: u8,
    mask: Option<RawMask>,
    divider: Option<Divider>,
    divider_blend: Option<[u8; 4]>,
    invert: bool,
    locked: bool,
    fill_opacity: u8,
}

impl RawLayer {
    fn has_color_channels(&self) -> bool {
        self.channels
            .iter()
            .any(|channel| (0..=2).contains(&channel.id))
    }
}

struct DecodeBudget {
    pixels: u64,
    mask_pixels: u64,
}

impl DecodeBudget {
    fn new() -> Self {
        Self {
            pixels: 0,
            mask_pixels: 0,
        }
    }

    fn add(&mut self, width: u32, height: u32) -> Result<()> {
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .context("PSD pixel count overflow")?;
        self.pixels = self
            .pixels
            .checked_add(pixels)
            .context("PSD pixel budget overflow")?;
        ensure!(
            self.pixels <= MAX_PIXELS,
            "PSD exceeds the 100 megapixel limit"
        );
        Ok(())
    }

    fn add_mask(&mut self, width: u32, height: u32) -> Result<()> {
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .context("PSD mask pixel budget overflow")?;
        self.mask_pixels = self
            .mask_pixels
            .checked_add(pixels)
            .context("PSD mask pixel budget overflow")?;
        ensure!(
            self.mask_pixels <= MAX_PIXELS,
            "PSD masks exceed the 100 megapixel limit"
        );
        Ok(())
    }

    fn ensure_peak(
        &self,
        width: u32,
        height: u32,
        has_color: bool,
        mask_size: Option<(u32, u32)>,
    ) -> Result<()> {
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .context("PSD decoded size overflow")?;
        let color_bytes = if has_color {
            pixels.checked_mul(8).context("PSD decoded size overflow")?
        } else {
            0
        };
        let mask_bytes = if let Some((mask_width, mask_height)) = mask_size {
            u64::from(mask_width)
                .checked_mul(u64::from(mask_height))
                .context("PSD mask decoded size overflow")?
        } else {
            0
        };
        let bytes = color_bytes
            .checked_add(mask_bytes)
            .context("PSD decoded size overflow")?;
        ensure!(
            bytes <= MAX_DECODE_BYTES,
            "PSD decoded data exceeds the memory limit"
        );
        Ok(())
    }
}

fn read_image_resources(data: &[u8]) -> Result<f32> {
    let mut reader = Reader::new(data);
    let mut resolution = 72.0_f32;
    while reader.remaining() > 0 {
        if reader.remaining() < 12 {
            ensure!(
                reader.data[reader.position..].iter().all(|byte| *byte == 0),
                "Invalid PSD image-resource padding"
            );
            reader.advance(reader.remaining())?;
            break;
        }
        let signature = reader.take(4)?;
        if signature != b"8BIM" && signature != b"8B64" {
            ensure!(
                signature.iter().all(|byte| *byte == 0),
                "Invalid PSD image-resource signature"
            );
            reader.advance(reader.remaining())?;
            break;
        }
        let id = reader.read_u16()?;
        let name_start = reader.position;
        let name_length = reader.read_u8()? as usize;
        reader.advance(name_length)?;
        let name_size = reader.position - name_start;
        let name_padding = (2 - name_size % 2) % 2;
        for _ in 0..name_padding {
            ensure!(
                reader.read_u8()? == 0,
                "Invalid PSD image-resource name padding"
            );
        }
        let length = u64::from(reader.read_u32()?);
        ensure!(length <= MAX_FILE_BYTES, "PSD image resource is too large");
        let resource = reader.read_block(length)?;
        if id == 1005 {
            ensure!(resource.data.len() >= 16, "Invalid PSD resolution resource");
            let horizontal = f64::from(u32::from_be_bytes([
                resource.data[0],
                resource.data[1],
                resource.data[2],
                resource.data[3],
            ])) / 65536.0;
            let unit = u16::from_be_bytes([resource.data[4], resource.data[5]]);
            let mut value = match unit {
                1 => horizontal,
                2 => horizontal * 72.0,
                _ => 72.0,
            };
            if !value.is_finite() || !(1.0..=9600.0).contains(&value) {
                value = 72.0;
            }
            resolution = value as f32;
        }
        if length % 2 != 0 {
            ensure!(reader.read_u8()? == 0, "Invalid PSD image-resource padding");
        }
    }
    Ok(resolution)
}

fn read_rect(reader: &mut Reader<'_>) -> Result<Rect> {
    Ok(Rect {
        top: reader.read_i32()?,
        left: reader.read_i32()?,
        bottom: reader.read_i32()?,
        right: reader.read_i32()?,
    })
}

fn read_pascal_name(reader: &mut Reader<'_>) -> Result<String> {
    let start = reader.position;
    let length = reader.read_u8()? as usize;
    let bytes = reader.take(length)?;
    let consumed = reader.position - start;
    let padding = (4 - consumed % 4) % 4;
    for _ in 0..padding {
        ensure!(reader.read_u8()? == 0, "Invalid PSD layer name padding");
    }
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn read_unicode_name(data: &[u8]) -> Result<String> {
    ensure!(data.len() >= 4, "Invalid PSD Unicode layer name");
    let count = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let byte_length = count
        .checked_mul(2)
        .context("PSD Unicode name is too large")?;
    ensure!(
        data.len() >= 4 + byte_length,
        "Truncated PSD Unicode layer name"
    );
    let units = data[4..4 + byte_length]
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    let name = String::from_utf16(&units).context("Invalid PSD Unicode layer name")?;
    let name = name.trim_end_matches('\0');
    ensure!(name.len() <= MAX_NAME_BYTES, "PSD layer name is too long");
    Ok(name.to_owned())
}

fn parse_mask(data: &[u8]) -> Result<Option<RawMask>> {
    if data.is_empty() {
        return Ok(None);
    }
    let mut reader = Reader::new(data);
    let rect = read_rect(&mut reader)?;
    rect.dimensions()?;
    let background = reader.read_u8()?;
    ensure!(
        background == 0 || background == 255,
        "Invalid PSD mask background"
    );
    let flags = reader.read_u8()?;
    ensure!(
        flags & 0x10 == 0,
        "PSD mask density and feather are not supported"
    );
    ensure!(
        reader.remaining() <= 3 && reader.data[reader.position..].iter().all(|byte| *byte == 0),
        "PSD mask metadata contains unsupported parameters"
    );
    Ok(Some(RawMask {
        rect,
        background,
        flags,
    }))
}

fn is_default_blending_ranges(data: &[u8]) -> bool {
    !data.is_empty()
        && data.len().is_multiple_of(4)
        && data.chunks_exact(4).all(|chunk| chunk == [0, 0, 255, 255])
}

fn read_tagged_blocks(
    reader: &mut Reader<'_>,
    variant: Variant,
    layer: &mut RawLayer,
) -> Result<()> {
    while reader.remaining() >= 12 {
        let signature = reader.take(4)?;
        if signature != b"8BIM" && signature != b"8B64" {
            ensure!(
                signature.iter().all(|byte| *byte == 0),
                "Invalid PSD tagged block signature"
            );
            reader.advance(reader.remaining())?;
            break;
        }
        let key: [u8; 4] = reader.take(4)?.try_into().unwrap();
        let wide_key = variant == Variant::Psb && is_wide_tagged_key(&key);
        let length = if wide_key {
            reader.read_u64()?
        } else {
            u64::from(reader.read_u32()?)
        };
        ensure!(
            !wide_key,
            "PSD 16/32-bit and extended layer metadata are not supported"
        );
        ensure!(length <= MAX_EXTRA_BYTES, "PSD tagged block is too large");
        let data = reader.take(usize::try_from(length)?)?;
        while reader.remaining() > 0 && reader.data[reader.position] == 0 {
            reader.advance(1)?;
        }
        match &key {
            b"lsct" => {
                ensure!(data.len() >= 4, "Invalid PSD section divider");
                let value = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                layer.divider = match value {
                    1 => Some(Divider::Open),
                    2 => Some(Divider::Closed),
                    3 => Some(Divider::Boundary),
                    0 => None,
                    _ => bail!("Unsupported PSD section divider {value}"),
                };
                if data.len() >= 12 {
                    ensure!(
                        &data[4..8] == b"8BIM" || &data[4..8] == b"8B64",
                        "Invalid PSD section divider blend signature"
                    );
                    layer.divider_blend = Some([data[8], data[9], data[10], data[11]]);
                }
            }
            b"luni" => {
                let name = read_unicode_name(data)?;
                if !name.is_empty() {
                    layer.name = name;
                }
            }
            b"iOpa" => {
                ensure!(!data.is_empty(), "Invalid PSD fill opacity");
                layer.fill_opacity = data[0];
            }
            b"nvrt" => layer.invert = true,
            b"clbl" => {
                ensure!(
                    data == [1, 0, 0, 0] || data.iter().all(|byte| *byte == 0),
                    "PSD channel blending restrictions are not supported"
                );
            }
            b"infx" => ensure!(
                data.iter().all(|byte| *byte == 0),
                "PSD interior elements are not supported"
            ),
            b"knko" => ensure!(
                data.iter().all(|byte| *byte == 0),
                "PSD knockout settings are not supported"
            ),
            b"fxrp" => ensure!(
                data.iter().all(|byte| *byte == 0),
                "PSD reference points are not supported"
            ),
            b"lspf" => {
                ensure!(data.len() >= 4, "Invalid PSD protection flags");
                let protection = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                layer.locked = protection == 0x8000_0000;
            }
            b"vmsk" | b"vsms" | b"vogk" | b"vstk" | b"vscg" => {
                bail!("PSD vector masks are not supported")
            }
            b"lfx2" | b"lfxs" | b"lmfx" | b"lrFX" => {
                bail!("PSD layer effects are not supported")
            }
            b"Txt2" | b"TySh" => bail!("PSD text layers are not supported"),
            b"SoCo" | b"GdFl" | b"PtFl" | b"SoLd" | b"SoLE" | b"PlLd" | b"PxSc" => {
                bail!("PSD fill, linked, and smart-object layers are not supported")
            }
            b"blwh" | b"blnc" | b"curv" | b"expA" | b"grdm" | b"hue2" | b"hue " | b"levl"
            | b"post" | b"vibA" | b"selc" | b"phfl" | b"thrs" | b"brit" | b"mixr" | b"clrL" => {
                bail!("PSD adjustment layers other than Invert are not supported")
            }
            b"lsdk" | b"brst" | b"artb" | b"abdd" | b"frgb" | b"CgEd" | b"vowv" => {
                bail!("PSD appearance metadata is not supported")
            }
            b"lnkD" | b"lnk2" | b"lnk3" | b"lnkE" => {
                bail!("PSD linked layers are not supported")
            }
            b"Lr16" | b"Lr32" | b"Layr" | b"Mt16" | b"Mt32" | b"Mtrn" | b"Alph" | b"FMsk"
            | b"Patt" | b"Pat2" | b"Pat3" | b"LMsk" | b"FXid" | b"FEid" | b"FELS" | b"PxSD"
            | b"pths" | b"extd" | b"cinf" | b"artd" => {
                bail!("PSD extended layer metadata is not supported")
            }
            b"lyid" | b"lyvr" | b"lclr" | b"lnsr" | b"shmd" | b"sn2P" => {}
            _ => bail!(
                "Unsupported PSD tagged block {:?}",
                String::from_utf8_lossy(&key)
            ),
        }
    }
    ensure!(
        reader.data[reader.position..].iter().all(|byte| *byte == 0),
        "Invalid PSD layer extra-data padding"
    );
    Ok(())
}

fn read_global_metadata(reader: &mut Reader<'_>, variant: Variant) -> Result<()> {
    if reader.remaining() == 0 {
        return Ok(());
    }
    let global_mask_length = u64::from(reader.read_u32()?);
    ensure!(
        global_mask_length == 0,
        "PSD global layer mask information is not supported"
    );
    while reader.remaining() >= 12 {
        let signature = reader.take(4)?;
        if signature != b"8BIM" && signature != b"8B64" {
            ensure!(
                signature.iter().all(|byte| *byte == 0),
                "Invalid PSD global tagged block signature"
            );
            reader.advance(reader.remaining())?;
            break;
        }
        let key: [u8; 4] = reader.take(4)?.try_into().unwrap();
        let wide_key = variant == Variant::Psb && is_wide_tagged_key(&key);
        let length = if wide_key {
            reader.read_u64()?
        } else {
            u64::from(reader.read_u32()?)
        };
        ensure!(
            !wide_key || &key == b"FMsk",
            "PSD extended global metadata is not supported"
        );
        ensure!(
            length <= MAX_EXTRA_BYTES,
            "PSD global tagged block is too large"
        );
        let data = reader.take(usize::try_from(length)?)?;
        while reader.remaining() > 0 && reader.data[reader.position] == 0 {
            reader.advance(1)?;
        }
        match &key {
            b"Patt" => ensure!(data.is_empty(), "PSD global patterns are not supported"),
            b"FMsk" => ensure!(
                data == [0, 0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 50],
                "PSD global filter masks are not supported"
            ),
            _ => bail!(
                "Unsupported PSD global tagged block {:?}",
                String::from_utf8_lossy(&key)
            ),
        }
    }
    ensure!(
        reader.data[reader.position..].iter().all(|byte| *byte == 0),
        "Invalid PSD global metadata padding"
    );
    Ok(())
}

fn read_layer_record(reader: &mut Reader<'_>, variant: Variant) -> Result<RawLayer> {
    let rect = read_rect(reader)?;
    rect.dimensions()?;
    let channel_count = reader.read_u16()? as usize;
    ensure!(
        channel_count <= MAX_CHANNELS,
        "PSD layer has too many channels"
    );
    let mut channels = Vec::with_capacity(channel_count);
    let mut channel_ids = HashSet::new();
    for _ in 0..channel_count {
        let id = reader.read_i16()?;
        ensure!(
            channel_ids.insert(id),
            "PSD layer contains duplicate channels"
        );
        ensure!(
            matches!(id, -2..=2),
            "PSD layer contains an unsupported channel"
        );
        let length = reader.read_length(variant.wide_lengths())?;
        ensure!(length <= MAX_FILE_BYTES, "PSD channel length is too large");
        channels.push(ChannelInfo { id, length });
    }
    ensure!(
        reader.take(4)? == b"8BIM",
        "Invalid PSD layer blend signature"
    );
    let blend_key: [u8; 4] = reader.take(4)?.try_into().unwrap();
    let opacity = reader.read_u8()?;
    let clipping = reader.read_u8()?;
    ensure!(clipping <= 1, "Invalid PSD layer clipping flag");
    let flags = reader.read_u8()?;
    let _filler = reader.read_u8()?;
    let extra_length = u64::from(reader.read_u32()?);
    ensure!(
        extra_length <= MAX_EXTRA_BYTES,
        "PSD layer extra data is too large"
    );
    let mut extra = reader.read_block(extra_length)?;
    let mask_length = u64::from(extra.read_u32()?);
    ensure!(
        mask_length <= MAX_EXTRA_BYTES,
        "PSD mask block is too large"
    );
    let mask_block = extra.read_block(mask_length)?;
    let mask = parse_mask(mask_block.data)?;
    let ranges_length = u64::from(extra.read_u32()?);
    ensure!(
        ranges_length <= MAX_EXTRA_BYTES,
        "PSD blending ranges are too large"
    );
    let ranges = extra.read_block(ranges_length)?;
    ensure!(
        ranges.data.is_empty() || is_default_blending_ranges(ranges.data),
        "PSD Blend If ranges are not supported"
    );
    let name = read_pascal_name(&mut extra)?;
    ensure!(name.len() <= MAX_NAME_BYTES, "PSD layer name is too long");
    let mut layer = RawLayer {
        rect,
        channels,
        name,
        blend_key,
        opacity,
        clipping,
        flags,
        mask,
        divider: None,
        divider_blend: None,
        invert: false,
        locked: false,
        fill_opacity: 255,
    };
    read_tagged_blocks(&mut extra, variant, &mut layer)?;
    Ok(layer)
}

fn read_record_count(reader: &mut Reader<'_>) -> Result<usize> {
    let count = reader.read_i16()?;
    let count = count.unsigned_abs() as usize;
    ensure!(count <= MAX_LAYERS, "PSD document has too many layers");
    Ok(count)
}

fn read_layer_records(
    reader: &mut Reader<'_>,
    variant: Variant,
    header: &Header,
    budget: &mut DecodeBudget,
) -> Result<Vec<DecodedLayer>> {
    let count = read_record_count(reader)?;
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        records.push(read_layer_record(reader, variant)?);
    }
    let mut decoded = Vec::with_capacity(records.len());
    for record in records {
        decoded.push(decode_layer(record, reader, header, budget)?);
    }
    Ok(decoded)
}

struct DecodedLayer {
    raw: RawLayer,
    pixels: Option<RgbaImage>,
    mask: Option<GrayImage>,
}

fn decode_packbits_row(data: &[u8], width: usize) -> Result<(Vec<u8>, usize)> {
    let mut result = Vec::with_capacity(width);
    let mut position = 0;
    while position < data.len() && result.len() < width {
        let control = data[position] as i8;
        position += 1;
        match control {
            -128 => {}
            -127..=-1 => {
                ensure!(position < data.len(), "PackBits repeat row is truncated");
                let count = 1 - i32::from(control);
                let count = usize::try_from(count)?;
                ensure!(result.len() + count <= width, "PackBits row overflow");
                result.extend(std::iter::repeat_n(data[position], count));
                position += 1;
            }
            0..=127 => {
                let count = usize::from(control as u8) + 1;
                ensure!(
                    position + count <= data.len(),
                    "PackBits literal row is truncated"
                );
                ensure!(result.len() + count <= width, "PackBits row overflow");
                result.extend_from_slice(&data[position..position + count]);
                position += count;
            }
        }
    }
    ensure!(result.len() == width, "PackBits row has the wrong width");
    Ok((result, position))
}

fn decode_channel_body(
    data: &[u8],
    compression: u16,
    width: u32,
    height: u32,
    variant: Variant,
) -> Result<(Vec<u8>, usize)> {
    let expected = usize::try_from(
        u64::from(width)
            .checked_mul(u64::from(height))
            .context("PSD channel size overflow")?,
    )?;
    match compression {
        0 => {
            ensure!(data.len() >= expected, "Truncated raw PSD channel");
            Ok((data[..expected].to_vec(), expected))
        }
        1 => {
            let row_size = if variant == Variant::Psd { 2 } else { 4 };
            let table_size = usize::try_from(u64::from(height) * u64::from(row_size as u32))?;
            ensure!(data.len() >= table_size, "Truncated PSD RLE row table");
            let mut position = table_size;
            let mut result = Vec::with_capacity(expected);
            for row_index in 0..height {
                let table_offset =
                    usize::try_from(u64::from(row_index) * u64::from(row_size as u32))?;
                let start = if variant == Variant::Psd {
                    u16::from_be_bytes([data[table_offset], data[table_offset + 1]]) as usize
                } else {
                    u32::from_be_bytes([
                        data[table_offset],
                        data[table_offset + 1],
                        data[table_offset + 2],
                        data[table_offset + 3],
                    ]) as usize
                };
                ensure!(
                    start <= data.len() - position,
                    "PSD RLE row length exceeds channel"
                );
                let (row, consumed) =
                    decode_packbits_row(&data[position..position + start], width as usize)?;
                ensure!(consumed == start, "PSD RLE row has trailing bytes");
                result.extend(row);
                position += start;
            }
            ensure!(
                result.len() == expected,
                "PSD RLE channel has the wrong size"
            );
            Ok((result, position))
        }
        2 => bail!("PSD ZIP compression is not supported"),
        3 => bail!("PSD ZIP prediction compression is not supported"),
        value => bail!("Unsupported PSD compression {value}"),
    }
}

fn decode_channel_blob(data: &[u8], width: u32, height: u32, variant: Variant) -> Result<Vec<u8>> {
    ensure!(
        data.len() >= 2,
        "PSD channel data is missing compression marker"
    );
    let compression = u16::from_be_bytes([data[0], data[1]]);
    let (values, consumed) = decode_channel_body(&data[2..], compression, width, height, variant)?;
    ensure!(consumed <= data.len() - 2, "PSD channel data is malformed");
    Ok(values)
}

fn channel_dimensions(layer: &RawLayer, id: i16) -> Result<(u32, u32)> {
    if (0..=2).contains(&id) || id == -1 {
        return layer.rect.dimensions();
    }
    if id == -2 {
        return layer
            .mask
            .as_ref()
            .context("PSD user mask has no mask metadata")?
            .rect
            .dimensions();
    }
    bail!("Unsupported PSD channel {id}")
}

fn build_mask(
    layer: &RawLayer,
    mask: &RawMask,
    values: &[u8],
    width: u32,
    height: u32,
    budget: &mut DecodeBudget,
) -> Result<GrayImage> {
    let (mask_width, mask_height) = mask.rect.dimensions()?;
    ensure!(
        values.len() == usize::try_from(u64::from(mask_width) * u64::from(mask_height))?,
        "PSD mask data has the wrong size"
    );
    budget.add_mask(width, height)?;
    let mut image = GrayImage::from_pixel(width, height, Luma([mask.background]));
    let relative = mask.flags & 1 != 0;
    let left = i64::from(mask.rect.left)
        + if relative {
            i64::from(layer.rect.left)
        } else {
            0
        };
    let top = i64::from(mask.rect.top)
        + if relative {
            i64::from(layer.rect.top)
        } else {
            0
        };
    for y in 0..mask_height {
        let document_y = top + i64::from(y);
        if document_y < 0 || document_y >= i64::from(height) {
            continue;
        }
        for x in 0..mask_width {
            let document_x = left + i64::from(x);
            if document_x < 0 || document_x >= i64::from(width) {
                continue;
            }
            let value = values[(y * mask_width + x) as usize];
            image.put_pixel(document_x as u32, document_y as u32, Luma([value]));
        }
    }
    if mask.flags & 4 != 0 {
        for pixel in image.pixels_mut() {
            pixel.0[0] = 255 - pixel.0[0];
        }
    }
    Ok(image)
}

fn decode_layer(
    raw: RawLayer,
    reader: &mut Reader<'_>,
    header: &Header,
    budget: &mut DecodeBudget,
) -> Result<DecodedLayer> {
    let declared_color = raw.has_color_channels();
    let (width, height) = raw.rect.dimensions()?;
    let marker = raw.divider.is_some() || raw.invert;
    if marker {
        ensure!(
            width == 0 && height == 0,
            "PSD group or adjustment marker contains pixels"
        );
    }
    let has_color = declared_color && !marker && width > 0 && height > 0;
    if !marker && has_color {
        ensure!(
            raw.channels.iter().any(|channel| channel.id == 0)
                && raw.channels.iter().any(|channel| channel.id == 1)
                && raw.channels.iter().any(|channel| channel.id == 2),
            "PSD RGB layer is missing a color channel"
        );
    } else if !marker && !declared_color {
        bail!("PSD layer has no RGB channels")
    }
    budget.ensure_peak(
        width,
        height,
        has_color,
        raw.mask.as_ref().map(|_| (header.width, header.height)),
    )?;
    let mut channels = HashMap::new();
    for channel in &raw.channels {
        let length = usize::try_from(channel.length)?;
        let data = reader
            .take(length)
            .with_context(|| format!("PSD channel {} data", channel.id))?;
        if channel.length == 0 {
            continue;
        }
        let (channel_width, channel_height) = channel_dimensions(&raw, channel.id)?;
        let values = decode_channel_blob(data, channel_width, channel_height, header.variant)
            .map_err(|error| anyhow::anyhow!("PSD channel {}: {error:#}", channel.id))?;
        channels.insert(channel.id, values);
    }
    if !marker && has_color {
        ensure!(
            channels.contains_key(&0) && channels.contains_key(&1) && channels.contains_key(&2),
            "PSD RGB layer is missing a color channel"
        );
    }
    let pixels = if has_color {
        budget.add(width, height)?;
        let red = channels.get(&0).context("PSD red channel is missing")?;
        let green = channels.get(&1).context("PSD green channel is missing")?;
        let blue = channels.get(&2).context("PSD blue channel is missing")?;
        let alpha = channels.get(&-1);
        let mut image = RgbaImage::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) as usize;
                image.put_pixel(
                    x,
                    y,
                    Rgba([
                        red[index],
                        green[index],
                        blue[index],
                        alpha.map_or(255, |values| values[index]),
                    ]),
                );
            }
        }
        Some(image)
    } else {
        None
    };
    let mask = if let Some(mask) = &raw.mask {
        if let Some(values) = channels.get(&-2) {
            Some(build_mask(
                &raw,
                mask,
                values,
                header.width,
                header.height,
                budget,
            )?)
        } else {
            let (mask_width, mask_height) = mask.rect.dimensions()?;
            ensure!(
                mask_width == 0 && mask_height == 0,
                "PSD mask metadata has no user mask channel"
            );
            None
        }
    } else {
        None
    };
    Ok(DecodedLayer { raw, pixels, mask })
}

fn blend_mode(
    key: [u8; 4],
    divider_blend: Option<[u8; 4]>,
    divider: Option<Divider>,
) -> Result<BlendMode> {
    if let Some(Divider::Open | Divider::Closed) = divider {
        let key = divider_blend.unwrap_or(key);
        ensure!(key == *b"pass", "PSD group blending is not supported");
        return Ok(BlendMode::Normal);
    }
    if let Some(Divider::Boundary) = divider {
        return Ok(BlendMode::Normal);
    }
    let mode = match &key {
        b"norm" => BlendMode::Normal,
        b"dark" => BlendMode::Darken,
        b"mul " => BlendMode::Multiply,
        b"idiv" => BlendMode::ColorBurn,
        b"lbrn" => BlendMode::LinearBurn,
        b"lite" => BlendMode::Lighten,
        b"scrn" => BlendMode::Screen,
        b"div " => BlendMode::ColorDodge,
        b"lddg" => BlendMode::LinearDodge,
        b"over" => BlendMode::Overlay,
        b"sLit" => BlendMode::SoftLight,
        b"hLit" => BlendMode::HardLight,
        b"vLit" => BlendMode::VividLight,
        b"lLit" => BlendMode::LinearLight,
        b"pLit" => BlendMode::PinLight,
        b"hMix" => BlendMode::HardMix,
        b"diff" => BlendMode::Difference,
        b"smud" => BlendMode::Exclusion,
        b"fsub" => BlendMode::Subtract,
        b"fdiv" => BlendMode::Divide,
        b"hue " => BlendMode::Hue,
        b"sat " => BlendMode::Saturation,
        b"colr" => BlendMode::Color,
        b"lum " => BlendMode::Luminosity,
        _ => bail!(
            "Unsupported PSD blend mode {:?}",
            String::from_utf8_lossy(&key)
        ),
    };
    Ok(mode)
}

struct GroupFrame {
    id: Uuid,
    parent: Option<Uuid>,
}

fn make_layer(
    decoded: DecodedLayer,
    document_width: u32,
    document_height: u32,
) -> Result<(Layer, u8)> {
    let raw = decoded.raw;
    let clipping = raw.clipping;
    let name = if raw.name.trim().is_empty() {
        "Layer".to_owned()
    } else {
        raw.name
    };
    let mut layer = if let Some(pixels) = decoded.pixels {
        let mut layer = Layer::image(name, pixels);
        layer.transform.x = raw.rect.left as f32;
        layer.transform.y = raw.rect.top as f32;
        layer
    } else {
        let mut layer = Layer::blank(name, document_width, document_height);
        if raw.invert {
            layer.adjustment = Some(Adjustment::Invert);
        } else if raw.divider.is_some() {
            layer.group = true;
        }
        layer
    };
    layer.visible = raw.flags & 2 == 0;
    layer.locked = raw.locked;
    layer.opacity = f32::from(raw.opacity) / 255.0 * f32::from(raw.fill_opacity) / 255.0;
    layer.blend = blend_mode(raw.blend_key, raw.divider_blend, raw.divider)?;
    layer.mask = decoded.mask.map(|pixels| Mask {
        pixels: Arc::new(pixels),
        enabled: raw.mask.as_ref().is_none_or(|mask| mask.flags & 2 == 0),
        linked: true,
        placement: Some(Transform::new(document_width, document_height)),
    });
    Ok((layer, clipping))
}

fn into_document(header: &Header, layers: Vec<DecodedLayer>, resolution: f32) -> Result<Document> {
    ensure!(!layers.is_empty(), "PSD document has no layers");
    let mut output = Vec::new();
    let mut groups: Vec<GroupFrame> = Vec::new();
    let mut bases: HashMap<Option<Uuid>, Option<Uuid>> = HashMap::new();
    for decoded in layers {
        let divider = decoded.raw.divider;
        let record_clipping = decoded.raw.clipping;
        if divider == Some(Divider::Boundary) {
            ensure!(
                record_clipping == 0,
                "PSD group marker has an invalid clipping source"
            );
            ensure!(
                groups.len() < MAX_GROUPS,
                "PSD document has too many nested groups"
            );
            let parent = groups.last().map(|group| group.id);
            bases.insert(parent, None);
            groups.push(GroupFrame {
                id: Uuid::new_v4(),
                parent,
            });
            continue;
        }
        let parent = groups.last().map(|group| group.id);
        if let Some(Divider::Open | Divider::Closed) = divider {
            let frame = groups
                .pop()
                .context("PSD group closing marker has no opening marker")?;
            let (mut layer, clipping) = make_layer(decoded, header.width, header.height)?;
            ensure!(clipping == 0, "PSD group has an invalid clipping source");
            layer.id = frame.id;
            layer.group = true;
            layer.parent = frame.parent;
            ensure!(
                layer.clip_to.is_none(),
                "PSD group has an invalid clipping source"
            );
            bases.insert(frame.parent, None);
            output.push(layer);
            continue;
        }
        let (mut layer, clipping) = make_layer(decoded, header.width, header.height)?;
        layer.parent = parent;
        if layer.group {
            ensure!(clipping == 0, "PSD group has an invalid clipping source");
            bases.insert(parent, None);
            output.push(layer);
            continue;
        }
        if clipping == 1 {
            let base = bases
                .get(&parent)
                .copied()
                .flatten()
                .context("PSD clipped layer has no supported base layer")?;
            layer.clip_to = Some(base);
        } else if layer.pixels.is_some() {
            bases.insert(parent, Some(layer.id));
        } else {
            bases.insert(parent, None);
        }
        output.push(layer);
    }
    ensure!(groups.is_empty(), "PSD document has an unclosed group");
    ensure!(!output.is_empty(), "PSD document has no convertible layers");
    let mut document = Document::new(header.width, header.height)?;
    document.resolution = resolution;
    document.layers = output;
    document.active = document.layers.last().map(|layer| layer.id);
    document.selected = document.active.into_iter().collect();
    document.validate()?;
    Ok(document)
}

fn decode_composite(reader: &mut Reader<'_>, header: &Header) -> Result<RgbaImage> {
    let compression = reader.read_u16()?;
    let mut planes = Vec::with_capacity(header.channels as usize);
    for _ in 0..header.channels {
        let start = reader.position;
        let remaining = &reader.data[start..];
        let (values, consumed) = decode_channel_body(
            remaining,
            compression,
            header.width,
            header.height,
            header.variant,
        )?;
        reader.position = start + consumed;
        planes.push(values);
    }
    let alpha = planes.get(3);
    let mut image = RgbaImage::new(header.width, header.height);
    for y in 0..header.height {
        for x in 0..header.width {
            let index = (y * header.width + x) as usize;
            image.put_pixel(
                x,
                y,
                Rgba([
                    planes[0][index],
                    planes[1][index],
                    planes[2][index],
                    alpha.map_or(255, |values| values[index]),
                ]),
            );
        }
    }
    Ok(image)
}

fn composite_layer(image: RgbaImage) -> DecodedLayer {
    let (width, height) = image.dimensions();
    DecodedLayer {
        raw: RawLayer {
            rect: Rect {
                top: 0,
                left: 0,
                bottom: height as i32,
                right: width as i32,
            },
            channels: Vec::new(),
            name: "Background".to_owned(),
            blend_key: *b"norm",
            opacity: 255,
            clipping: 0,
            flags: 0,
            mask: None,
            divider: None,
            divider_blend: None,
            invert: false,
            locked: false,
            fill_opacity: 255,
        },
        pixels: Some(image),
        mask: None,
    }
}

pub fn is_document(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let extension = extension.to_ascii_lowercase();
            extension == "psd" || extension == "psb"
        })
}

pub fn load(path: &Path) -> Result<Document> {
    let metadata = fs::metadata(path).with_context(|| format!("Cannot read {}", path.display()))?;
    ensure!(metadata.len() <= MAX_FILE_BYTES, "PSD file exceeds 512 MiB");
    let data = fs::read(path)?;
    ensure!(
        data.len() as u64 <= MAX_FILE_BYTES,
        "PSD file exceeds 512 MiB"
    );
    parse(&data)
}

fn parse(data: &[u8]) -> Result<Document> {
    let mut reader = Reader::new(data);
    let header = read_header(&mut reader)?;
    let color_data_length = u64::from(reader.read_u32()?);
    ensure!(
        color_data_length <= MAX_FILE_BYTES,
        "PSD color data is too large"
    );
    reader.advance(usize::try_from(color_data_length)?)?;
    let resources_length = u64::from(reader.read_u32()?);
    ensure!(
        resources_length <= MAX_FILE_BYTES,
        "PSD image resources are too large"
    );
    let resources = reader.read_block(resources_length)?;
    let resolution = read_image_resources(resources.data)?;
    let layer_mask_length = reader.read_length(header.variant.wide_lengths())?;
    let mut budget = DecodeBudget::new();
    let mut layers = Vec::new();
    if layer_mask_length > 0 {
        ensure!(
            layer_mask_length <= MAX_FILE_BYTES,
            "PSD layer section is too large"
        );
        let mut layer_mask = reader.read_block(layer_mask_length)?;
        let layer_info_length = layer_mask.read_length(header.variant.wide_lengths())?;
        if layer_info_length > 0 {
            ensure!(
                layer_info_length <= MAX_FILE_BYTES,
                "PSD layer info is too large"
            );
            let mut layer_info = layer_mask.read_block(layer_info_length)?;
            layers = read_layer_records(&mut layer_info, header.variant, &header, &mut budget)?;
        }
        read_global_metadata(&mut layer_mask, header.variant)?;
    }
    if layers.is_empty() {
        budget.ensure_peak(header.width, header.height, true, None)?;
        let image = decode_composite(&mut reader, &header)?;
        budget.add(header.width, header.height)?;
        layers.push(composite_layer(image));
    }
    into_document(&header, layers, resolution)
}

#[cfg(test)]
mod tests;
