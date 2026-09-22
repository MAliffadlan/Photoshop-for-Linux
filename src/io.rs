use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use image::{DynamicImage, ImageFormat, ImageReader, RgbaImage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    blend::BlendMode,
    document::{
        Adjustment, ColorOverlayEffect, Document, Guide, GuideAxis, InnerShadowEffect, Layer,
        LayerEffects, MAX_PIXELS, Mask, OuterGlowEffect, Point, ShadowEffect, StrokeEffect,
        Transform, validate_size,
    },
    render,
};

const MAX_MANIFEST: u64 = 4 * 1024 * 1024;
const MAX_ASSET: u64 = 512 * 1024 * 1024;

/// Format identifier written into `manifest.json`.
const FORMAT_ID: &str = "me.silverl.mectov";
/// Formats accepted when loading. The second is the identifier written before the project was
/// renamed; files carrying it stay loadable, which is the only reason it is still listed here.
const READ_FORMATS: [&str; 2] = [FORMAT_ID, "me.silverl.xuan"];
/// Project extensions recognized on disk, newest first. The earlier extension still opens.
const PROJECT_EXTENSIONS: [&str; 2] = ["mectov", "xuan"];

#[derive(Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u32,
    document: Document,
    pixel_layers: HashSet<Uuid>,
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>> {
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, ImageFormat::Png)?;
    Ok(encoded.into_inner())
}

fn decode_image(bytes: Vec<u8>, used: &mut u64) -> Result<DynamicImage> {
    ensure!(bytes.len() as u64 <= MAX_ASSET, "Image file is too large");
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(30_000);
    limits.max_image_height = Some(30_000);
    limits.max_alloc = Some(MAX_PIXELS * 8);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    use image::ImageDecoder;
    let (width, height) = decoder.dimensions();
    validate_size(width, height)?;
    *used += u64::from(width) * u64::from(height);
    ensure!(
        *used <= MAX_PIXELS,
        "Project exceeds 100 megapixels of source images"
    );
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok(image)
}

pub fn import_image(path: &Path) -> Result<RgbaImage> {
    let metadata = fs::metadata(path).with_context(|| format!("Cannot read {}", path.display()))?;
    ensure!(metadata.len() <= MAX_ASSET, "Image exceeds 512 MiB");
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "heic" | "heif" | "hif") {
        let temporary = tempfile::tempdir()?;
        let output = temporary.path().join("image.png");
        let result = std::process::Command::new("heif-convert")
            .arg(path.canonicalize()?)
            .arg(&output)
            .output()
            .context("HEIC import requires heif-convert (install the libheif-examples package)")?;
        ensure!(
            result.status.success(),
            "HEIC conversion failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        // Collections may be emitted as image-1.png, image-2.png, and so on.
        let mut files: Vec<_> = fs::read_dir(temporary.path())?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "png"))
            .collect();
        files.sort();
        let decoded = if output.exists() {
            &output
        } else {
            files.first().context("HEIC decoder produced no image")?
        };
        return import_image(decoded);
    }
    Ok(decode_image(fs::read(path)?, &mut 0)?.to_rgba8())
}

/// Persist a complete sibling temporary file, then atomically replace the destination.
pub fn save(document: &Document, path: &Path) -> Result<()> {
    document.validate()?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut archive = ZipWriter::new(temporary.as_file_mut());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        // Guides and layer effects are version 3: older readers must not silently drop them.
        let version =
            if !document.guides.is_empty() || document.layers.iter().any(|l| l.effects.is_some()) {
                3
            } else if document.layers.iter().any(|l| l.raw.is_some()) {
                2
            } else {
                1
            };
        let manifest = Manifest {
            format: FORMAT_ID.into(),
            version,
            document: document.clone(),
            pixel_layers: document
                .layers
                .iter()
                .filter(|l| l.pixels.is_some())
                .map(|l| l.id)
                .collect(),
        };
        let json = serde_json::to_vec_pretty(&manifest)?;
        ensure!(
            json.len() as u64 <= MAX_MANIFEST,
            "Project metadata exceeds 4 MiB"
        );
        archive.start_file("manifest.json", options)?;
        archive.write_all(&json)?;
        for layer in &document.layers {
            if let Some(raw) = &layer.raw {
                archive.start_file(format!("raw/{}.nef", layer.id), options)?;
                archive.write_all(&raw.bytes)?;
            }
            if let Some(pixels) = &layer.pixels {
                archive.start_file(format!("images/{}.png", layer.id), options)?;
                archive.write_all(&encode_png(&DynamicImage::ImageRgba8((**pixels).clone()))?)?;
            }
            if let Some(mask) = &layer.mask {
                archive.start_file(format!("images/{}.mask.png", layer.id), options)?;
                archive.write_all(&encode_png(&DynamicImage::ImageLuma8(
                    (*mask.pixels).clone(),
                ))?)?;
            }
        }
        archive.finish()?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn zip_read(archive: &mut ZipArchive<File>, name: &str, limit: u64) -> Result<Vec<u8>> {
    let file = archive
        .by_name(name)
        .with_context(|| format!("Missing project asset: {name}"))?;
    ensure!(file.size() <= limit, "Project asset exceeds size limit");
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Project asset exceeds size limit"
    );
    Ok(bytes)
}

/// Whether a path carries a mectov project extension, including files saved before the rename.
pub fn is_project_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| PROJECT_EXTENSIONS.iter().any(|known| extension == *known))
}

pub fn load(path: &Path) -> Result<Document> {
    if path.is_dir() {
        return load_compositor(path);
    }
    let mut archive = ZipArchive::new(File::open(path)?)?;
    ensure!(archive.len() <= 30_001, "Too many project assets");
    let mut manifest: Manifest =
        serde_json::from_slice(&zip_read(&mut archive, "manifest.json", MAX_MANIFEST)?)?;
    ensure!(
        READ_FORMATS.contains(&manifest.format.as_str()) && (1..=3).contains(&manifest.version),
        "Unsupported mectov project version"
    );
    let mut used_pixels = 0;
    let mut used_masks = 0;
    let mut used_raw = 0;
    ensure!(manifest.document.layers.len() <= 10_000, "Too many layers");
    validate_size(manifest.document.width, manifest.document.height)?;
    for layer in &mut manifest.document.layers {
        if let Some(raw) = &mut layer.raw {
            let bytes = zip_read(&mut archive, &format!("raw/{}.nef", layer.id), MAX_ASSET)?;
            used_raw += bytes.len() as u64;
            ensure!(
                used_raw <= crate::raw::MAX_RAW_BYTES,
                "Project exceeds 512 MiB of RAW assets"
            );
            raw.bytes = Arc::new(bytes);
            raw.validate()?;
        }
        if manifest.pixel_layers.contains(&layer.id) {
            let bytes = zip_read(&mut archive, &format!("images/{}.png", layer.id), MAX_ASSET)?;
            layer.pixels = Some(Arc::new(decode_image(bytes, &mut used_pixels)?.to_rgba8()));
        }
        if let Some(mask) = &mut layer.mask {
            let bytes = zip_read(
                &mut archive,
                &format!("images/{}.mask.png", layer.id),
                MAX_ASSET,
            )?;
            mask.pixels = Arc::new(decode_image(bytes, &mut used_masks)?.to_luma8());
        }
    }
    ensure!(
        manifest
            .pixel_layers
            .iter()
            .all(|id| manifest.document.layers.iter().any(|l| l.id == *id)),
        "Unreferenced pixel layer metadata"
    );
    manifest.document.selected = manifest.document.active.into_iter().collect();
    manifest.document.validate()?;
    Ok(manifest.document)
}

fn package_read(root: &Path, relative: &Path, limit: u64) -> Result<Vec<u8>> {
    let root = root.canonicalize()?;
    let file = root.join(relative);
    let metadata = fs::symlink_metadata(&file)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= limit,
        "Unsafe or oversized project asset"
    );
    ensure!(
        file.canonicalize()?.starts_with(&root),
        "Project asset escapes its package"
    );
    let mut bytes = Vec::new();
    File::open(file)?.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Project asset exceeds size limit"
    );
    Ok(bytes)
}

fn number(value: &Value, key: &str, default: f32) -> f32 {
    value[key].as_f64().map_or(default, |v| v as f32)
}
fn identifier(value: &Value) -> Result<Option<Uuid>> {
    value
        .as_str()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(Into::into)
}

fn comp_transform(value: &Value) -> Result<Transform> {
    let pair = |value: &Value, a: &str, b: &str| -> Result<(f32, f32)> {
        let (x, y) = if let Some(values) = value.as_array() {
            ensure!(values.len() == 2, "Invalid transform coordinates");
            (values[0].as_f64(), values[1].as_f64())
        } else {
            (value[a].as_f64(), value[b].as_f64())
        };
        Ok((
            x.context("Missing transform coordinate")? as f32,
            y.context("Missing transform coordinate")? as f32,
        ))
    };
    let (x, y) = pair(&value["origin"], "x", "y")?;
    let (width, height) = pair(&value["size"], "width", "height")?;
    let t = Transform {
        x,
        y,
        width,
        height,
        rotation: number(value, "rotation", 0.0),
        flip_x: value["flipX"].as_bool().unwrap_or(false),
        flip_y: value["flipY"].as_bool().unwrap_or(false),
        warp: None,
    };
    ensure!(t.valid(), "Invalid Compositor layer transform");
    Ok(t)
}

// Swift dictionaries with enum keys are encoded as alternating key/value arrays.
fn swift_dictionary_get<'a>(value: &'a Value, key: &str) -> &'a Value {
    if let Some(array) = value.as_array() {
        for pair in array.as_chunks::<2>().0 {
            if pair[0].as_str() == Some(key) {
                return &pair[1];
            }
        }
        &Value::Null
    } else {
        &value[key]
    }
}

fn comp_adjustment(value: &Value) -> Result<Adjustment> {
    let kind = value["kind"]
        .as_str()
        .context("Adjustment kind is missing")?;
    let result = match kind {
        "Hue/Saturation" => {
            let hsv = &value["hsvSettings"];
            if hsv.is_null() {
                Adjustment::HueSaturation {
                    hue: number(value, "hue", 0.0),
                    saturation: number(value, "saturation", 0.0),
                    lightness: number(value, "lightness", 0.0),
                    colorize: value["colorize"].as_bool().unwrap_or(false),
                }
            } else {
                let mut settings = crate::color::HueSettings {
                    range: crate::color::HueSettings::RANGES
                        .iter()
                        .position(|name| Some(*name) == hsv["range"].as_str())
                        .unwrap_or(0),
                    colorize: hsv["colorize"].as_bool().unwrap_or(false),
                    invert_range: hsv["invertRange"].as_bool().unwrap_or(false),
                    ..Default::default()
                };
                for (index, name) in crate::color::HueSettings::RANGES.iter().enumerate() {
                    let adjustment = swift_dictionary_get(&hsv["adjustments"], name);
                    settings.adjustments[index] = [
                        number(adjustment, "hue", 0.0),
                        number(adjustment, "saturation", 0.0),
                        number(adjustment, "lightness", 0.0),
                    ];
                    let band = swift_dictionary_get(&hsv["bands"], name);
                    if !band.is_null() {
                        settings.bands[index] = [
                            number(band, "falloffStart", 0.0),
                            number(band, "rangeStart", 0.0),
                            number(band, "rangeEnd", 360.0),
                            number(band, "falloffEnd", 360.0),
                        ];
                    }
                }
                Adjustment::HueRanges {
                    settings: Box::new(settings),
                }
            }
        }
        "Levels" => {
            let input = value["levels"]["ranges"]
                .as_array()
                .context("Missing levels ranges")?;
            ensure!(input.len() == 4, "Invalid levels ranges");
            let ranges = std::array::from_fn(|i| {
                let r = &input[i];
                [
                    number(r, "black", 0.0),
                    number(r, "gamma", 1.0),
                    number(r, "white", 255.0),
                    number(r, "outputBlack", 0.0),
                    number(r, "outputWhite", 255.0),
                ]
            });
            Adjustment::LevelsChannels { ranges }
        }
        "Curves" => {
            let input = value["curves"]["channels"]
                .as_array()
                .context("Missing curve channels")?;
            ensure!(
                input.len() == 4 && input.iter().all(|c| c.is_array()),
                "Invalid curve channels"
            );
            let channels = std::array::from_fn(|i| {
                input[i]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| Point::new(number(p, "x", 0.0) / 255.0, number(p, "y", 0.0) / 255.0))
                    .collect()
            });
            Adjustment::CurvesChannels { channels }
        }
        "Exposure" => {
            let settings = &value["exposureSettings"];
            Adjustment::Exposure {
                exposure: number(settings, "exposure", 0.0),
                offset: number(settings, "offset", 0.0),
                gamma: number(settings, "gamma", 1.0),
            }
        }
        "Gradient Map" => {
            let color = |key| {
                let c = &value["gradientMapSettings"][key];
                [
                    number(c, "red", if key == "shadows" { 0.0 } else { 1.0 }),
                    number(c, "green", if key == "shadows" { 0.0 } else { 1.0 }),
                    number(c, "blue", if key == "shadows" { 0.0 } else { 1.0 }),
                    1.0,
                ]
                .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
            };
            Adjustment::GradientMap {
                shadows: color(
                    if value["gradientMapSettings"]["reversed"]
                        .as_bool()
                        .unwrap_or(false)
                    {
                        "highlights"
                    } else {
                        "shadows"
                    },
                ),
                highlights: color(
                    if value["gradientMapSettings"]["reversed"]
                        .as_bool()
                        .unwrap_or(false)
                    {
                        "shadows"
                    } else {
                        "highlights"
                    },
                ),
            }
        }
        "Grain" => {
            let settings = &value["grainSettings"];
            Adjustment::FilmGrain {
                amount: number(settings, "amount", 0.0),
                size: number(settings, "size", 1.0),
                roughness: number(settings, "roughness", 50.0),
                seed: settings["seed"].as_u64().unwrap_or(1) as u32,
            }
        }
        // Compositor 1.2.2's three additions. Invert has nothing to set, and the other two carry
        // the settings Photoshop opens with, so an absent key keeps its default.
        "Invert" => Adjustment::Invert,
        "Black & White" => {
            let settings = &value["blackWhiteSettings"];
            Adjustment::BlackWhite {
                reds: number(settings, "reds", 40.0),
                yellows: number(settings, "yellows", 60.0),
                greens: number(settings, "greens", 40.0),
                cyans: number(settings, "cyans", 60.0),
                blues: number(settings, "blues", 20.0),
                magentas: number(settings, "magentas", 80.0),
                tint: settings["tint"].as_bool().unwrap_or(false),
                tint_hue: number(settings, "tintHue", 40.0),
                tint_saturation: number(settings, "tintSaturation", 20.0),
            }
        }
        "Color Balance" => {
            let settings = &value["colorBalanceSettings"];
            let pair = |prefix: &str| {
                [
                    number(settings, &format!("{prefix}CyanRed"), 0.0),
                    number(settings, &format!("{prefix}MagentaGreen"), 0.0),
                    number(settings, &format!("{prefix}YellowBlue"), 0.0),
                ]
            };
            Adjustment::ColorBalance {
                shadows: pair("shadow"),
                midtones: pair("mid"),
                highlights: pair("highlight"),
                preserve_luminosity: settings["preserveLuminosity"].as_bool().unwrap_or(true),
            }
        }
        _ => bail!("Unsupported Compositor adjustment: {kind}"),
    };
    crate::effects::validate_adjustment(&result)?;
    Ok(result)
}

fn comp_color(value: &Value, default: [f32; 3]) -> [f32; 3] {
    [
        number(value, "red", default[0]),
        number(value, "green", default[1]),
        number(value, "blue", default[2]),
    ]
}

/// Compositor hides a disabled effect instead of deleting it, and a missing flag means visible.
fn comp_enabled(value: &Value) -> bool {
    value["enabled"].as_bool().unwrap_or(true)
}

/// Read the layer effects Compositor 1.1 added. Keys a project leaves out take the source's defaults, so the
/// effect keeps the values Compositor would show.
fn comp_effects(value: &Value) -> Result<Option<LayerEffects>> {
    fn present(value: &Value, key: &str) -> bool {
        value[key].as_object().is_some()
    }
    let effects = LayerEffects {
        stroke: present(value, "stroke").then(|| {
            let default = StrokeEffect::default();
            let stroke = &value["stroke"];
            StrokeEffect {
                enabled: comp_enabled(stroke),
                size: number(stroke, "size", default.size),
                color: comp_color(stroke, default.color),
                opacity: number(stroke, "opacity", default.opacity),
                inside: stroke["inside"].as_bool().unwrap_or(default.inside),
            }
        }),
        shadow: present(value, "shadow").then(|| {
            let default = ShadowEffect::default();
            let shadow = &value["shadow"];
            ShadowEffect {
                enabled: comp_enabled(shadow),
                angle: number(shadow, "angle", default.angle),
                distance: number(shadow, "distance", default.distance),
                blur: number(shadow, "blur", default.blur),
                color: comp_color(shadow, default.color),
                opacity: number(shadow, "opacity", default.opacity),
            }
        }),
        color_overlay: present(value, "colorOverlay").then(|| {
            let default = ColorOverlayEffect::default();
            let overlay = &value["colorOverlay"];
            ColorOverlayEffect {
                enabled: comp_enabled(overlay),
                color: comp_color(overlay, default.color),
                opacity: number(overlay, "opacity", default.opacity),
            }
        }),
        inner_shadow: present(value, "innerShadow").then(|| {
            let default = InnerShadowEffect::default();
            let shadow = &value["innerShadow"];
            InnerShadowEffect {
                enabled: comp_enabled(shadow),
                angle: number(shadow, "angle", default.angle),
                distance: number(shadow, "distance", default.distance),
                blur: number(shadow, "blur", default.blur),
                color: comp_color(shadow, default.color),
                opacity: number(shadow, "opacity", default.opacity),
            }
        }),
        outer_glow: present(value, "outerGlow").then(|| {
            let default = OuterGlowEffect::default();
            let glow = &value["outerGlow"];
            OuterGlowEffect {
                enabled: comp_enabled(glow),
                size: number(glow, "size", default.size),
                color: comp_color(glow, default.color),
                opacity: number(glow, "opacity", default.opacity),
            }
        }),
    };
    if effects.is_empty() {
        return Ok(None);
    }
    effects.validate()?;
    Ok(Some(effects))
}

pub fn load_compositor(path: &Path) -> Result<Document> {
    let manifest: Value = serde_json::from_slice(&package_read(
        path,
        Path::new("manifest.json"),
        MAX_MANIFEST,
    )?)?;
    ensure!(
        manifest["format"] == "com.compositor.project",
        "Not a Compositor project"
    );
    let version = manifest["version"]
        .as_u64()
        .context("Project version missing")?;
    ensure!(
        (1..=8).contains(&version),
        "Unsupported Compositor project version {version}"
    );
    ensure!(
        manifest["colorSpace"].as_str().unwrap_or("sRGB") == "sRGB",
        "Unsupported color space"
    );
    let width = u32::try_from(manifest["width"].as_u64().context("Missing canvas width")?)?;
    let height = u32::try_from(
        manifest["height"]
            .as_u64()
            .context("Missing canvas height")?,
    )?;
    let mut document = Document::new(width, height)?;
    document.id = identifier(&manifest["documentID"])?.context("Missing document ID")?;
    document.resolution = number(&manifest, "resolution", 72.0);
    document.layers.clear();
    let records = manifest["layers"].as_array().context("Missing layers")?;
    ensure!(records.len() <= 10_000, "Too many layers");
    let mut image_pixels = 0;
    let mut mask_pixels = 0;
    for record in records {
        let id = identifier(&record["id"])?.context("Missing layer ID")?;
        let mut layer = Layer::blank(
            record["name"].as_str().context("Missing layer name")?,
            width,
            height,
        );
        layer.id = id;
        layer.visible = record["isVisible"].as_bool().unwrap_or(true);
        layer.transform = comp_transform(&record["transform"])?;
        layer.parent = identifier(&record["parentID"])?;
        layer.group = record["isGroup"].as_bool().unwrap_or(false);
        layer.opacity = number(record, "opacity", 1.0);
        let blend_name = record["blendMode"].as_str().unwrap_or("Normal");
        layer.blend = BlendMode::ALL
            .into_iter()
            .find(|b| b.name() == blend_name)
            .context("Unknown blend mode")?;
        layer.clip_to = identifier(&record["maskSourceID"])?;
        if let Some(name) = record["imageFile"].as_str() {
            ensure!(
                name.eq_ignore_ascii_case(&format!("{id}.png")),
                "Unsafe layer asset path"
            );
            let bytes = package_read(path, &Path::new("images").join(name), MAX_ASSET)?;
            layer.pixels = Some(Arc::new(decode_image(bytes, &mut image_pixels)?.to_rgba8()));
        }
        if let Some(name) = record["maskFile"].as_str() {
            ensure!(
                name.eq_ignore_ascii_case(&format!("{id}.mask.png")),
                "Unsafe mask asset path"
            );
            let bytes = package_read(path, &Path::new("images").join(name), MAX_ASSET)?;
            layer.mask = Some(Mask {
                pixels: Arc::new(decode_image(bytes, &mut mask_pixels)?.to_luma8()),
                enabled: record["maskEnabled"].as_bool().unwrap_or(true),
                linked: record["maskLinked"].as_bool().unwrap_or(true),
                placement: if record["maskPlacement"].is_null() {
                    None
                } else {
                    Some(comp_transform(&record["maskPlacement"])?)
                },
            });
        }
        // A line shape's pixels live in the layer asset. mectov has no live line shape yet, so the layer stays
        // plain pixels rather than redrawing as a rectangle.
        if let Some(shape) = record["shape"]
            .as_object()
            .filter(|_| record["shape"]["kind"].as_str() != Some("Line"))
        {
            let radius = shape
                .get("cornerRadius")
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as f32;
            ensure!(radius.is_finite() && radius >= 0.0, "Invalid shape radius");
            let kind = if record["shape"]["kind"] == "Ellipse" {
                crate::paint::ShapeKind::Ellipse
            } else if radius > 0.0 {
                crate::paint::ShapeKind::RoundedRectangle
            } else {
                crate::paint::ShapeKind::Rectangle
            };
            let color =
                |key| (number(&record["shape"], key, 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            layer.shape = Some(crate::document::ShapeStyle {
                kind,
                color: [color("red"), color("green"), color("blue"), 255],
                corner_radius: radius,
            });
        }
        if !record["adjustment"].is_null() {
            layer.adjustment = Some(comp_adjustment(&record["adjustment"])?);
        }
        if let Some(effects) = comp_effects(&record["effects"])? {
            layer.effects = Some(effects);
        }
        document.layers.push(layer);
    }
    // Guides arrived in version 8; earlier projects have no such key.
    if let Some(guides) = manifest["guides"].as_array() {
        ensure!(
            guides.len() <= crate::document::MAX_GUIDES,
            "Too many guides"
        );
        for guide in guides {
            let axis = match guide["axis"].as_str().unwrap_or("horizontal") {
                "horizontal" => GuideAxis::Horizontal,
                "vertical" => GuideAxis::Vertical,
                other => bail!("Unknown guide axis: {other}"),
            };
            document.guides.push(Guide {
                axis,
                position: number(guide, "position", 0.0),
            });
        }
    }
    document.active = identifier(&manifest["activeLayerID"])?;
    document.selected = document.active.into_iter().collect();
    document.validate()?;
    Ok(document)
}

pub fn export(document: &Document, path: &Path, quality: u8) -> Result<()> {
    let image = render::render(document);
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_lowercase();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    match extension.as_str() {
        "jpg" | "jpeg" => {
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                temporary.as_file_mut(),
                quality.clamp(1, 100),
            );
            encoder.set_pixel_density(image::codecs::jpeg::PixelDensity::dpi(
                document.resolution.round() as u16,
            ));
            encoder.encode_image(&render::flatten_white(&image))?;
        }
        "png" => {
            let mut encoder =
                png::Encoder::new(temporary.as_file_mut(), image.width(), image.height());
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let pixels_per_meter = (document.resolution / 0.0254).round() as u32;
            encoder.set_pixel_dims(Some(png::PixelDimensions {
                xppu: pixels_per_meter,
                yppu: pixels_per_meter,
                unit: png::Unit::Meter,
            }));
            encoder.write_header()?.write_image_data(image.as_raw())?;
        }
        "tif" | "tiff" => {
            DynamicImage::ImageRgba8(image).write_to(temporary.as_file_mut(), ImageFormat::Tiff)?
        }
        "webp" => {
            DynamicImage::ImageRgba8(image).write_to(temporary.as_file_mut(), ImageFormat::WebP)?
        }
        _ => bail!("Export as PNG, JPEG, TIFF or WebP"),
    }
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma, Rgba};

    #[test]
    fn exports_all_formats_and_png_print_resolution() {
        let temporary = tempfile::tempdir().unwrap();
        let mut doc = Document::new(12, 8).unwrap();
        doc.resolution = 300.0;
        doc.layers[0].pixels = Some(Arc::new(RgbaImage::from_pixel(
            12,
            8,
            Rgba([180, 90, 30, 128]),
        )));
        for extension in ["png", "jpg", "tiff", "webp"] {
            let path = temporary.path().join(format!("image.{extension}"));
            export(&doc, &path, 95).unwrap();
            let image = import_image(&path).unwrap();
            assert_eq!(image.dimensions(), (12, 8));
            if extension == "jpg" {
                assert_eq!(image.get_pixel(0, 0)[3], 255);
            } else {
                assert_eq!(image.get_pixel(0, 0)[3], 128);
            }
        }
        let decoder = png::Decoder::new(std::io::BufReader::new(
            File::open(temporary.path().join("image.png")).unwrap(),
        ));
        let reader = decoder.read_info().unwrap();
        let density = reader.info().pixel_dims.unwrap();
        assert_eq!(density.xppu, 11811);
        assert_eq!(density.unit, png::Unit::Meter);
    }

    #[test]
    fn imports_swift_enum_dictionaries_and_individual_color_channels() {
        let value = serde_json::json!({"kind":"Hue/Saturation", "hsvSettings": {
            "range":"Reds", "colorize":false, "invertRange":true,
            "adjustments":["Master", {"hue":5,"saturation":0,"lightness":0}, "Reds", {"hue":40,"saturation":-20,"lightness":3}],
            "bands":["Reds", {"falloffStart":310,"rangeStart":340,"rangeEnd":20,"falloffEnd":50}]
        }});
        let Adjustment::HueRanges { settings } = comp_adjustment(&value).unwrap() else {
            panic!("expected selective hue settings");
        };
        assert_eq!(settings.range, 1);
        assert_eq!(settings.adjustments[1], [40.0, -20.0, 3.0]);
        assert_eq!(settings.bands[1], [310.0, 340.0, 20.0, 50.0]);
        assert!(settings.invert_range);
        let default =
            serde_json::json!({"black":0,"gamma":1,"white":255,"outputBlack":0,"outputWhite":255});
        let mut value = serde_json::json!({"kind":"Levels","levels":{"ranges":[default,default,default,default]}});
        value["levels"]["ranges"][1]["gamma"] = serde_json::json!(1.5);
        let Adjustment::LevelsChannels { ranges } = comp_adjustment(&value).unwrap() else {
            panic!("expected channel levels");
        };
        assert_eq!(ranges[1][1], 1.5);
    }

    #[test]
    fn portable_project_round_trip_and_atomic_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.mectov");
        let mut doc = Document::new(3, 2).unwrap();
        doc.layers[0].pixels = Some(Arc::new(RgbaImage::from_pixel(
            3,
            2,
            Rgba([20, 40, 80, 128]),
        )));
        doc.layers[0].mask = Some(Mask {
            pixels: Arc::new(GrayImage::from_pixel(3, 2, Luma([128]))),
            ..Mask::white()
        });
        save(&doc, &path).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(render::render(&doc), render::render(&loaded));
        doc.layers[0].name = "Renamed".into();
        save(&doc, &path).unwrap();
        assert_eq!(load(&path).unwrap().layers[0].name, "Renamed");
        doc.width = 0;
        assert!(save(&doc, &path).is_err());
        assert_eq!(load(&path).unwrap().width, 3);
    }

    #[test]
    fn loads_projects_saved_before_the_rename() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.xuan");
        let manifest = serde_json::json!({
            "format": "me.silverl.xuan",
            "version": 1,
            "document": Document::new(2, 2).unwrap(),
            "pixel_layers": [],
        });
        let mut archive = ZipWriter::new(File::create(&path).unwrap());
        archive
            .start_file("manifest.json", SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        archive.finish().unwrap();
        assert_eq!(load(&path).unwrap().width, 2);
    }

    #[test]
    fn recognizes_current_and_legacy_project_extensions() {
        for name in ["photo.mectov", "photo.xuan"] {
            assert!(is_project_path(Path::new(name)), "{name}");
        }
        for name in ["photo.png", "photo", "photo.mectov.bak"] {
            assert!(!is_project_path(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn imports_swift_transform_and_rejects_path_traversal() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("images")).unwrap();
        let id = Uuid::new_v4();
        let mut value = serde_json::json!({"format":"com.compositor.project", "version":7, "documentID":Uuid::new_v4(), "width":2, "height":2, "activeLayerID":id,
            "layers":[{"id":id,"name":"Test", "isVisible":true,"transform":{"origin":[1,2],"size":[2,2],"rotation":30,"flipX":true,"flipY":false}}]});
        let manifest = directory.path().join("manifest.json");
        fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
        let document = load_compositor(directory.path()).unwrap();
        assert_eq!(document.layers[0].transform.rotation, 30.0);
        assert!(document.layers[0].transform.flip_x);
        value["layers"][0]["imageFile"] = Value::String("../../outside.png".into());
        fs::write(manifest, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(load_compositor(directory.path()).is_err());
    }

    /// The version 8 manifest Compositor 1.2 writes: guides on the document, layer effects on a layer.
    fn compositor_v8_manifest(id: Uuid) -> Value {
        serde_json::json!({
            "format": "com.compositor.project",
            "version": 8,
            "colorSpace": "sRGB",
            "documentID": Uuid::new_v4(),
            "width": 2,
            "height": 2,
            "activeLayerID": id,
            "guides": [
                {"id": Uuid::new_v4(), "axis": "vertical", "position": 12.5},
                {"id": Uuid::new_v4(), "axis": "horizontal", "position": 4.0},
            ],
            "layers": [{
                "id": id,
                "name": "Test",
                "isVisible": true,
                "transform": {"origin": [0, 0], "size": [2, 2], "rotation": 0, "flipX": false, "flipY": false},
                "blendMode": "Soft Light",
                "effects": {
                    "stroke": {"size": 6, "red": 0.25, "green": 0.5, "blue": 1.0, "opacity": 0.8, "inside": true},
                    "shadow": {"angle": 45, "distance": 12, "blur": 8, "red": 0, "green": 0, "blue": 0, "opacity": 0.4},
                    "outerGlow": {"enabled": false, "size": 30, "red": 1, "green": 1, "blue": 0, "opacity": 0.6},
                },
            }],
        })
    }

    #[test]
    fn imports_version_eight_projects_with_guides_and_effects() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = compositor_v8_manifest(Uuid::new_v4());
        fs::write(
            directory.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let document = load_compositor(directory.path()).unwrap();
        assert_eq!(
            document.guides,
            [
                Guide {
                    axis: GuideAxis::Vertical,
                    position: 12.5,
                },
                Guide {
                    axis: GuideAxis::Horizontal,
                    position: 4.0,
                },
            ]
        );
        assert_eq!(document.layers[0].blend, BlendMode::SoftLight);
        let effects = document.layers[0].effects.unwrap();
        assert_eq!(
            effects.stroke,
            Some(StrokeEffect {
                enabled: true,
                size: 6.0,
                color: [0.25, 0.5, 1.0],
                opacity: 0.8,
                inside: true,
            })
        );
        assert_eq!(
            effects.shadow,
            Some(ShadowEffect {
                enabled: true,
                angle: 45.0,
                distance: 12.0,
                blur: 8.0,
                color: [0.0; 3],
                opacity: 0.4,
            })
        );
        // A disabled effect keeps its values, exactly as the source hides rather than deletes it.
        assert_eq!(
            effects.outer_glow,
            Some(OuterGlowEffect {
                enabled: false,
                size: 30.0,
                color: [1.0, 1.0, 0.0],
                opacity: 0.6,
            })
        );
        assert_eq!(effects.color_overlay, None);
        assert_eq!(effects.inner_shadow, None);
        // The app opens a `.comp` package through the same entry point as a `.mectov` file.
        assert_eq!(load(directory.path()).unwrap().guides, document.guides);
    }

    /// Compositor 1.2.2 widened the blend menu to Photoshop's set; every name it writes must
    /// import, since an unknown one fails the whole project.
    #[test]
    fn imports_every_compositor_blend_mode() {
        let directory = tempfile::tempdir().unwrap();
        let ids: Vec<Uuid> = BlendMode::ALL.iter().map(|_| Uuid::new_v4()).collect();
        let layers: Vec<Value> = BlendMode::ALL
            .iter()
            .zip(&ids)
            .map(|(mode, id)| {
                serde_json::json!({
                    "id": id,
                    "name": mode.name(),
                    "isVisible": true,
                    "transform": {"origin": [0, 0], "size": [2, 2], "rotation": 0, "flipX": false, "flipY": false},
                    "blendMode": mode.name(),
                })
            })
            .collect();
        let mut manifest = compositor_v8_manifest(ids[0]);
        manifest["activeLayerID"] = serde_json::json!(ids[0]);
        manifest["layers"] = Value::Array(layers);
        fs::write(
            directory.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let document = load_compositor(directory.path()).unwrap();
        let blends: Vec<BlendMode> = document.layers.iter().map(|layer| layer.blend).collect();
        assert_eq!(blends, BlendMode::ALL.to_vec());
    }

    /// Compositor 1.2.2's three additions. An unknown kind fails the whole project, so each one
    /// has to decode with the settings Photoshop opens with, and a project carrying them imports.
    #[test]
    fn imports_compositor_12_2_adjustments() {
        assert_eq!(
            comp_adjustment(&serde_json::json!({"kind": "Invert"})).unwrap(),
            Adjustment::Invert
        );
        assert_eq!(
            comp_adjustment(
                &serde_json::json!({"kind": "Black & White", "blackWhiteSettings": {
                    "reds": 70, "tint": true, "tintHue": 200, "tintSaturation": 45
                }})
            )
            .unwrap(),
            Adjustment::BlackWhite {
                reds: 70.0,
                yellows: 60.0,
                greens: 40.0,
                cyans: 60.0,
                blues: 20.0,
                magentas: 80.0,
                tint: true,
                tint_hue: 200.0,
                tint_saturation: 45.0,
            }
        );
        assert_eq!(
            comp_adjustment(
                &serde_json::json!({"kind": "Color Balance", "colorBalanceSettings": {
                    "shadowCyanRed": 25, "highlightYellowBlue": -30, "preserveLuminosity": false
                }})
            )
            .unwrap(),
            Adjustment::ColorBalance {
                shadows: [25.0, 0.0, 0.0],
                midtones: [0.0; 3],
                highlights: [0.0, 0.0, -30.0],
                preserve_luminosity: false,
            }
        );
        // An absent settings block still decodes: that is how a project written before these kinds
        // gained their options reads.
        let Adjustment::BlackWhite {
            reds,
            tint_hue,
            tint_saturation,
            ..
        } = comp_adjustment(&serde_json::json!({"kind": "Black & White"})).unwrap()
        else {
            panic!("expected Black & White");
        };
        assert_eq!((reds, tint_hue, tint_saturation), (40.0, 40.0, 20.0));
        // Settings outside the source's ranges are rejected rather than imported.
        assert!(
            comp_adjustment(
                &serde_json::json!({"kind": "Black & White", "blackWhiteSettings":
                {"reds": 400}})
            )
            .is_err()
        );

        let directory = tempfile::tempdir().unwrap();
        let ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        let layers: Vec<Value> = ["Invert", "Black & White", "Color Balance"]
            .iter()
            .zip(&ids)
            .map(|(kind, id)| {
                serde_json::json!({
                    "id": id,
                    "name": kind,
                    "isVisible": true,
                    "transform": {"origin": [0, 0], "size": [2, 2], "rotation": 0, "flipX": false, "flipY": false},
                    "blendMode": "Normal",
                    "adjustment": {"kind": kind},
                })
            })
            .collect();
        let mut manifest = compositor_v8_manifest(ids[0]);
        manifest["activeLayerID"] = serde_json::json!(ids[0]);
        manifest["layers"] = Value::Array(layers);
        fs::write(
            directory.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let document = load_compositor(directory.path()).unwrap();
        let kinds: Vec<&str> = document
            .layers
            .iter()
            .map(|layer| layer.adjustment.as_ref().unwrap().name())
            .collect();
        assert_eq!(kinds, ["Invert", "Black & White", "Color Balance"]);
    }

    #[test]
    fn rejects_later_project_versions_and_invalid_effects() {
        let directory = tempfile::tempdir().unwrap();
        let manifest_path = directory.path().join("manifest.json");
        let id = Uuid::new_v4();
        let mut manifest = compositor_v8_manifest(id);
        manifest["version"] = serde_json::json!(9);
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(
            load_compositor(directory.path()).unwrap_err().to_string(),
            "Unsupported Compositor project version 9"
        );
        manifest["version"] = serde_json::json!(8);
        manifest["layers"][0]["effects"]["stroke"]["opacity"] = serde_json::json!(2.0);
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(
            load_compositor(directory.path()).unwrap_err().to_string(),
            "Invalid stroke color"
        );
    }

    #[test]
    fn imported_guides_and_effects_survive_a_mectov_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("effects.mectov");
        let manifest = compositor_v8_manifest(Uuid::new_v4());
        fs::write(
            directory.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let document = load_compositor(directory.path()).unwrap();
        save(&document, &path).unwrap();
        let mut archive = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let stored: Value =
            serde_json::from_slice(&zip_read(&mut archive, "manifest.json", MAX_MANIFEST).unwrap())
                .unwrap();
        assert_eq!(stored["version"], 3);
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.guides, document.guides);
        assert_eq!(loaded.layers[0].effects, document.layers[0].effects);
    }

    #[test]
    fn line_shapes_import_as_pixels_and_ordinary_shapes_stay_live() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("images")).unwrap();
        let id = Uuid::new_v4();
        let pixels = RgbaImage::from_pixel(2, 2, Rgba([255, 255, 255, 255]));
        fs::write(
            directory.path().join(format!("images/{id}.png")),
            encode_png(&DynamicImage::ImageRgba8(pixels)).unwrap(),
        )
        .unwrap();
        let mut manifest = serde_json::json!({"format":"com.compositor.project", "version":8, "documentID":Uuid::new_v4(), "width":2, "height":2, "activeLayerID":id,
            "layers":[{"id":id,"name":"Line", "isVisible":true,"transform":{"origin":[0,0],"size":[2,2],"rotation":0,"flipX":false,"flipY":false},
                "imageFile": format!("{id}.png"),
                "shape":{"kind":"Line","red":1,"green":1,"blue":1,"cornerRadius":0,"lineWidth":3,"start":[0,0],"end":[1,1]}}]});
        fs::write(
            directory.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let document = load_compositor(directory.path()).unwrap();
        assert!(document.layers[0].shape.is_none());
        assert!(document.layers[0].pixels.is_some());
        manifest["layers"][0]["shape"] =
            serde_json::json!({"kind":"Ellipse","red":0,"green":0,"blue":0,"cornerRadius":0});
        fs::write(
            directory.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let document = load_compositor(directory.path()).unwrap();
        assert_eq!(
            document.layers[0].shape.as_ref().map(|shape| shape.kind),
            Some(crate::paint::ShapeKind::Ellipse)
        );
    }
}
