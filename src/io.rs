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

use crate::memory;
use crate::{
    blend::BlendMode,
    document::{
        Adjustment, ColorOverlayEffect, Document, Guide, GuideAxis, InnerGlowEffect,
        InnerShadowEffect, Layer, LayerEffects, MAX_DOCUMENT_PIXELS, MAX_SIDE, Mask,
        OuterGlowEffect, Point, ShadowEffect, StrokeEffect, Transform, limit_message,
        max_document_pixels, max_image_pixels, validate_size,
    },
    render,
};

const MAX_MANIFEST: u64 = 4 * 1024 * 1024;
/// The largest single asset a project may hold. One image of 200 megapixels
/// cannot fit in a 512 MiB file, so this follows the image limit, and is capped
/// so that a machine which cannot hold a gigabyte spare does not try.
const MAX_ASSET: u64 = 1024 * 1024 * 1024;
/// The share of the machine's memory one asset file read may take, in quarters.
const ASSET_MEMORY_SHARE: u64 = 1;
const MAX_SVG_BYTES: u64 = 64 * 1024 * 1024;

/// The largest asset file this port reads, which is a quarter of the machine's
/// memory while that is below the ceiling.
pub fn max_asset_bytes() -> u64 {
    max_asset_bytes_for(memory::total_bytes())
}

/// The largest asset file a machine with `total` memory is read with, which
/// takes a machine other than this one so that the ceiling can be tested without
/// one.
pub fn max_asset_bytes_for(total: u64) -> u64 {
    MAX_ASSET.min(memory::bytes_for(total, ASSET_MEMORY_SHARE))
}

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
    let allowed_asset = max_asset_bytes();
    ensure!(
        bytes.len() as u64 <= allowed_asset,
        "Image file needs {} of memory, and this machine allows {}",
        memory::gibibytes(bytes.len() as u64),
        memory::gibibytes(allowed_asset)
    );
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    // The decoder's own ceiling is measured against the same pixels this port
    // allows, with room for the copy a decoder works from.
    limits.max_alloc = Some(max_image_pixels() * 8);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    use image::ImageDecoder;
    let (width, height) = decoder.dimensions();
    validate_size(width, height)?;
    *used += u64::from(width) * u64::from(height);
    let allowed = max_document_pixels();
    ensure!(
        *used <= allowed,
        "{}",
        limit_message("The images in a project", allowed, MAX_DOCUMENT_PIXELS)
    );
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok(image)
}

fn decode_svg(data: &[u8]) -> Result<RgbaImage> {
    ensure!(data.len() as u64 <= MAX_SVG_BYTES, "SVG exceeds 64 MiB");
    let decompressed = if data.starts_with(&[0x1f, 0x8b]) {
        let decoder = flate2::read::GzDecoder::new(data);
        let mut decoded = Vec::new();
        decoder
            .take(MAX_SVG_BYTES + 1)
            .read_to_end(&mut decoded)
            .map_err(|_| anyhow::anyhow!("Invalid SVGZ"))?;
        ensure!(decoded.len() as u64 <= MAX_SVG_BYTES, "SVGZ exceeds 64 MiB");
        Some(decoded)
    } else {
        None
    };
    let data = decompressed.as_deref().unwrap_or(data);
    let options = resvg::usvg::Options {
        resources_dir: None,
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: resvg::usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(data, &options)
        .map_err(|error| anyhow::anyhow!("Invalid SVG: {error}"))?;
    let size = tree.size();
    ensure!(
        size.width().is_finite()
            && size.height().is_finite()
            && size.width() > 0.0
            && size.height() > 0.0,
        "SVG has invalid dimensions"
    );
    let width = size.width().ceil() as u32;
    let height = size.height().ceil() as u32;
    validate_size(width, height)?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .context("SVG dimensions exceed the rasterizer limits")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    RgbaImage::from_raw(width, height, pixmap.take_demultiplied())
        .context("SVG rasterizer returned an invalid image")
}

pub fn import_image(path: &Path) -> Result<RgbaImage> {
    let metadata = fs::metadata(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let allowed_asset = max_asset_bytes();
    ensure!(
        metadata.len() <= allowed_asset,
        "Image needs {} of memory, and this machine allows {}",
        memory::gibibytes(metadata.len()),
        memory::gibibytes(allowed_asset)
    );
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(extension.as_str(), "svg" | "svgz") {
        ensure!(metadata.len() <= MAX_SVG_BYTES, "SVG exceeds 64 MiB");
        return decode_svg(&fs::read(path)?);
    }
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
        // Guides, layer effects, the 1.2.3 blur and noise adjustment layers, the layout grid,
        // paragraph text boxes, separately coloured letters, the colour grading wheels and the
        // effects and calibration that follow them all raise the version, because an older reader
        // must refuse the file rather than silently drop what it cannot draw. A developed camera
        // file is the one that cannot be recovered from the saved pixels: the layer holds the
        // original, and everything above is only in here.
        let version = if document.layers.iter().any(|layer| {
            layer
                .raw
                .as_ref()
                .is_some_and(|raw| raw.settings.adjusts_effects())
        }) {
            10
        } else if document.layers.iter().any(|layer| {
            layer
                .raw
                .as_ref()
                .is_some_and(|raw| raw.settings.grading.adjusts())
        }) {
            9
        } else if document.layers.iter().any(|layer| {
            layer
                .text
                .as_ref()
                .is_some_and(|text| !text.color_runs.is_empty())
        }) {
            8
        } else if document
            .layers
            .iter()
            .any(|layer| layer.text.as_ref().is_some_and(|text| text.r#box.is_some()))
        {
            7
        } else if document.grid.is_some() {
            6
        } else if document.layers.iter().any(|layer| {
            layer
                .shape
                .as_ref()
                .is_some_and(|shape| shape.kind == crate::paint::ShapeKind::Line)
        }) {
            5
        } else if document.layers.iter().any(|layer| {
            layer.adjustment.as_ref().is_some_and(|adjustment| {
                matches!(
                    adjustment,
                    Adjustment::GaussianBlur { .. }
                        | Adjustment::MotionBlur { .. }
                        | Adjustment::AddNoise { .. }
                )
            })
        }) {
            4
        } else if !document.guides.is_empty() || document.layers.iter().any(|l| l.effects.is_some())
        {
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
        READ_FORMATS.contains(&manifest.format.as_str()) && (1..=10).contains(&manifest.version),
        "Unsupported mectov project version"
    );
    let mut used_pixels = 0;
    let mut used_masks = 0;
    let mut used_raw = 0;
    ensure!(manifest.document.layers.len() <= 10_000, "Too many layers");
    validate_size(manifest.document.width, manifest.document.height)?;
    for layer in &mut manifest.document.layers {
        if let Some(raw) = &mut layer.raw {
            let bytes = zip_read(
                &mut archive,
                &format!("raw/{}.nef", layer.id),
                max_asset_bytes(),
            )?;
            used_raw += bytes.len() as u64;
            ensure!(
                used_raw <= crate::raw::MAX_RAW_BYTES,
                "Project exceeds 512 MiB of RAW assets"
            );
            raw.bytes = Arc::new(bytes);
            raw.validate()?;
        }
        if manifest.pixel_layers.contains(&layer.id) {
            let bytes = zip_read(
                &mut archive,
                &format!("images/{}.png", layer.id),
                max_asset_bytes(),
            )?;
            layer.pixels = Some(Arc::new(decode_image(bytes, &mut used_pixels)?.to_rgba8()));
        }
        if let Some(mask) = &mut layer.mask {
            let bytes = zip_read(
                &mut archive,
                &format!("images/{}.mask.png", layer.id),
                max_asset_bytes(),
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

fn comp_point(value: &Value, key: &str) -> Result<Option<Point>> {
    let value = &value[key];
    if value.is_null() {
        return Ok(None);
    }
    let (x, y) = if let Some(values) = value.as_array() {
        ensure!(values.len() == 2, "Invalid line endpoint");
        (values[0].as_f64(), values[1].as_f64())
    } else {
        (value["x"].as_f64(), value["y"].as_f64())
    };
    let point = Point::new(
        x.context("Missing line endpoint")? as f32,
        y.context("Missing line endpoint")? as f32,
    );
    ensure!(
        point.x.is_finite()
            && point.y.is_finite()
            && (0.0..=1.0).contains(&point.x)
            && (0.0..=1.0).contains(&point.y),
        "Invalid line endpoint"
    );
    Ok(Some(point))
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
        // Compositor 1.2.3's three additions. Each key is optional there, so an absent one keeps the
        // value Core Image opens with, and a project that leaves them out imports unchanged.
        "Add Noise" => Adjustment::AddNoise {
            amount: number(value, "noiseAmount", 10.0),
            gaussian: value["noiseGaussian"].as_bool().unwrap_or(false),
            monochromatic: value["noiseMonochromatic"].as_bool().unwrap_or(false),
            seed: value["noiseSeed"].as_u64().unwrap_or(0) as u32,
        },
        "Gaussian Blur" => Adjustment::GaussianBlur {
            radius: number(value, "blurRadius", 10.0),
        },
        "Motion Blur" => Adjustment::MotionBlur {
            angle: number(value, "motionAngle", 0.0),
            distance: number(value, "motionDistance", 10.0),
        },
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
        // Compositor 1.2.3 added this one; the field keeps it so a project can be saved back without
        // the glow quietly disappearing, even though nothing draws layer effects yet.
        inner_glow: present(value, "innerGlow").then(|| {
            let default = InnerGlowEffect::default();
            let glow = &value["innerGlow"];
            InnerGlowEffect {
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
    // Compositor 1.2.3 writes 9, which adds blur and noise adjustment layers to 1.2.0's version 8,
    // and 1.3.2 writes 10, which can colour some letters of a text layer differently. A text layer
    // arrives as the pixels Compositor rendered, so its metadata is not read at any version.
    ensure!(
        (1..=10).contains(&version),
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
            let bytes = package_read(path, &Path::new("images").join(name), max_asset_bytes())?;
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
        if record["shape"].is_object() {
            let shape = &record["shape"];
            let radius = number(shape, "cornerRadius", 0.0);
            ensure!(radius.is_finite() && radius >= 0.0, "Invalid shape radius");
            let kind = if shape["kind"] == "Ellipse" {
                crate::paint::ShapeKind::Ellipse
            } else if shape["kind"] == "Line" {
                crate::paint::ShapeKind::Line
            } else if radius > 0.0 {
                crate::paint::ShapeKind::RoundedRectangle
            } else {
                crate::paint::ShapeKind::Rectangle
            };
            let color = |key| (number(shape, key, 0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            let line_width =
                (kind == crate::paint::ShapeKind::Line).then(|| number(shape, "lineWidth", 1.0));
            if let Some(line_width) = line_width {
                ensure!(
                    line_width.is_finite()
                        && (0.0..=crate::document::MAX_SHAPE_SIZE).contains(&line_width),
                    "Invalid line width"
                );
            }
            layer.shape = Some(crate::document::ShapeStyle {
                kind,
                color: [color("red"), color("green"), color("blue"), 255],
                corner_radius: radius,
                line_width,
                start: if kind == crate::paint::ShapeKind::Line {
                    comp_point(shape, "start")?
                } else {
                    None
                },
                end: if kind == crate::paint::ShapeKind::Line {
                    comp_point(shape, "end")?
                } else {
                    None
                },
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
    use crate::document::GridSettings;
    use image::{GrayImage, Luma, Rgba};

    fn saved_version(path: &Path) -> u64 {
        let mut archive = ZipArchive::new(File::open(path).unwrap()).unwrap();
        let manifest: Value =
            serde_json::from_slice(&zip_read(&mut archive, "manifest.json", MAX_MANIFEST).unwrap())
                .unwrap();
        manifest["version"].as_u64().unwrap()
    }

    #[test]
    fn imports_svg_with_intrinsic_dimensions_and_blocks_external_images() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shape.svg");
        fs::write(
            &path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="3"><rect width="4" height="3" fill="#ff0000"/><image href="outside.png" x="0" y="0" width="4" height="3"/></svg>"##,
        )
        .unwrap();
        let image = import_image(&path).unwrap();
        assert_eq!(image.dimensions(), (4, 3));
        assert_eq!(image.get_pixel(2, 1).0, [255, 0, 0, 255]);
    }

    #[test]
    fn imports_svgz_data() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(br##"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="#00ff00"/></svg>"##)
            .unwrap();
        let compressed = encoder.finish().unwrap();
        let image = decode_svg(&compressed).unwrap();
        assert_eq!(image.dimensions(), (2, 2));
        assert_eq!(image.get_pixel(0, 0).0, [0, 255, 0, 255]);
    }

    #[test]
    fn rejects_invalid_svg_before_rasterizing() {
        assert!(decode_svg(b"<svg><rect/>").is_err());
        assert!(decode_svg(b"not an svg").is_err());
    }

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
    fn compositor_manifest(id: Uuid, version: u64) -> Value {
        serde_json::json!({
            "format": "com.compositor.project",
            "version": version,
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

    fn compositor_v8_manifest(id: Uuid) -> Value {
        compositor_manifest(id, 8)
    }

    /// A project as Compositor 1.2.3 writes one: version 9, Inner Glow on the layer that carries the
    /// effects, and the three adjustment kinds the source only allows from this version on.
    fn compositor_v9_manifest() -> (Value, Vec<Uuid>) {
        let ids: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        let mut manifest = compositor_manifest(ids[0], 9);
        manifest["activeLayerID"] = serde_json::json!(ids[0]);
        manifest["layers"][0]["effects"]["innerGlow"] =
            serde_json::json!({"size": 24, "red": 0.5, "green": 0.75, "blue": 1.0, "opacity": 0.9});
        let mut layers = manifest["layers"].as_array().unwrap().clone();
        layers.extend(
            ["Gaussian Blur", "Motion Blur", "Add Noise"]
                .iter()
                .zip(&ids[1..])
                .map(|(kind, id)| {
                    serde_json::json!({
                        "id": id,
                        "name": kind,
                        "isVisible": true,
                        "transform": {"origin": [0, 0], "size": [2, 2], "rotation": 0, "flipX": false, "flipY": false},
                        "blendMode": "Normal",
                        "adjustment": {"kind": kind},
                    })
                }),
        );
        manifest["layers"] = Value::Array(layers);
        (manifest, ids)
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

    /// Compositor 1.2.3 writes version 9. A project that carries the kinds it added has to import
    /// whole, and everything the blur and noise layers set has to arrive with it.
    #[test]
    fn imports_version_nine_projects_with_blur_and_noise_layers() {
        assert_eq!(
            comp_adjustment(&serde_json::json!({"kind": "Gaussian Blur"})).unwrap(),
            Adjustment::GaussianBlur { radius: 10.0 }
        );
        assert_eq!(
            comp_adjustment(&serde_json::json!({
                "kind": "Motion Blur", "motionAngle": -45, "motionDistance": 120
            }))
            .unwrap(),
            Adjustment::MotionBlur {
                angle: -45.0,
                distance: 120.0,
            }
        );
        assert_eq!(
            comp_adjustment(&serde_json::json!({
                "kind": "Add Noise", "noiseAmount": 65, "noiseGaussian": true,
                "noiseMonochromatic": true, "noiseSeed": 99
            }))
            .unwrap(),
            Adjustment::AddNoise {
                amount: 65.0,
                gaussian: true,
                monochromatic: true,
                seed: 99,
            }
        );
        // Keys the source leaves optional keep the settings its own adjustments open with.
        assert_eq!(
            comp_adjustment(&serde_json::json!({"kind": "Add Noise"})).unwrap(),
            Adjustment::AddNoise {
                amount: 10.0,
                gaussian: false,
                monochromatic: false,
                seed: 0,
            }
        );
        // Settings outside the source's ranges are refused with the project, not imported.
        assert!(
            comp_adjustment(&serde_json::json!({"kind": "Gaussian Blur", "blurRadius": 900}))
                .is_err()
        );
        assert!(
            comp_adjustment(&serde_json::json!({"kind": "Motion Blur", "motionAngle": 180}))
                .is_err()
        );

        let directory = tempfile::tempdir().unwrap();
        let (manifest, ids) = compositor_v9_manifest();
        fs::write(
            directory.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let document = load_compositor(directory.path()).unwrap();
        let kinds: Vec<&str> = document
            .layers
            .iter()
            .filter_map(|layer| layer.adjustment.as_ref())
            .map(|adjustment| adjustment.name())
            .collect();
        assert_eq!(kinds, ["Gaussian Blur", "Motion Blur", "Add Noise"]);
        assert_eq!(
            document.layers[0].effects.unwrap().inner_glow,
            Some(InnerGlowEffect {
                enabled: true,
                size: 24.0,
                color: [0.5, 0.75, 1.0],
                opacity: 0.9,
            })
        );
        // The blur layers name the ids the project gave them, and the whole set round trips as version
        // 4 of mectov's own format, which is the version that understands these kinds.
        let path = directory.path().join("blur.mectov");
        save(&document, &path).unwrap();
        let mut archive = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let stored: Value =
            serde_json::from_slice(&zip_read(&mut archive, "manifest.json", MAX_MANIFEST).unwrap())
                .unwrap();
        assert_eq!(stored["version"], 4);
        let loaded = load(&path).unwrap();
        for (before, after) in document.layers.iter().zip(&loaded.layers) {
            assert_eq!(before.id, after.id);
            assert_eq!(before.adjustment, after.adjustment);
            assert_eq!(before.effects, after.effects);
        }
        assert_eq!(loaded.layers[1].id, ids[1]);
    }

    #[test]
    fn rejects_later_project_versions_and_invalid_effects() {
        let directory = tempfile::tempdir().unwrap();
        let manifest_path = directory.path().join("manifest.json");
        let id = Uuid::new_v4();
        let mut manifest = compositor_v8_manifest(id);
        manifest["version"] = serde_json::json!(11);
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(
            load_compositor(directory.path()).unwrap_err().to_string(),
            "Unsupported Compositor project version 11"
        );
        // Compositor 1.3.2 writes 10, which opens: a text layer arrives as the
        // pixels Compositor rendered, so its letter colours change nothing here.
        manifest["version"] = serde_json::json!(10);
        manifest["layers"][0]["text"] = serde_json::json!({
            "content": "Coloured",
            "colorRuns": [{ "location": 0, "length": 3, "red": 1.0, "green": 0.0, "blue": 0.0 }]
        });
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let loaded = load_compositor(directory.path()).unwrap();
        assert!(
            loaded.layers[0].text.is_none(),
            "a .comp text layer is imported as the pixels it was saved with"
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
    fn grid_projects_write_version_six_and_older_features_keep_their_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grid.mectov");
        let mut document = Document::new(64, 64).unwrap();
        save(&document, &path).unwrap();
        assert_eq!(saved_version(&path), 1);
        document.guides.push(Guide {
            axis: GuideAxis::Vertical,
            position: 8.0,
        });
        save(&document, &path).unwrap();
        assert_eq!(saved_version(&path), 3);
        assert_eq!(load(&path).unwrap().grid, None);
        document.grid = Some(GridSettings {
            spacing: 16.0,
            subdivisions: 4,
        });
        save(&document, &path).unwrap();
        assert_eq!(saved_version(&path), 6);
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.grid, document.grid);
        assert_eq!(loaded.guides, document.guides);
    }

    #[test]
    fn reads_version_seven_projects_and_refuses_later_ones() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("seven.mectov");
        let mut document = Document::new(2, 2).unwrap();
        document.grid = Some(GridSettings {
            spacing: 8.0,
            subdivisions: 2,
        });
        let write = |manifest: &Value, layer: Option<Uuid>| {
            let mut archive = ZipWriter::new(File::create(&path).unwrap());
            archive
                .start_file("manifest.json", SimpleFileOptions::default())
                .unwrap();
            archive
                .write_all(&serde_json::to_vec(manifest).unwrap())
                .unwrap();
            if let Some(layer) = layer {
                archive
                    .start_file(format!("images/{layer}.png"), SimpleFileOptions::default())
                    .unwrap();
                archive
                    .write_all(
                        &encode_png(&DynamicImage::from(RgbaImage::from_pixel(
                            120,
                            40,
                            Rgba([0, 0, 0, 0]),
                        )))
                        .unwrap(),
                    )
                    .unwrap();
            }
            archive.finish().unwrap();
        };
        let mut manifest = serde_json::json!({
            "format": FORMAT_ID,
            "version": 6,
            "document": document,
            "pixel_layers": [],
        });
        write(&manifest, None);
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.grid.unwrap().minor_spacing(), 4.0);

        manifest["document"]["grid"] = serde_json::json!({"spacing": 0.0, "subdivisions": 1});
        write(&manifest, None);
        assert_eq!(load(&path).unwrap_err().to_string(), "Invalid grid spacing");

        // Version 7 carries a paragraph box, whose fields fill in their defaults.
        manifest["version"] = serde_json::json!(7);
        manifest["document"]["grid"] = serde_json::json!({"spacing": 8.0, "subdivisions": 1});
        let layer_id = Uuid::new_v4();
        manifest["pixel_layers"] = serde_json::json!([layer_id]);
        manifest["document"]["active"] = serde_json::json!(layer_id);
        manifest["document"]["layers"] = serde_json::json!([{
            "id": layer_id,
            "name": "Text",
            "visible": true,
            "locked": false,
            "opacity": 1.0,
            "blend": "Normal",
            "transform": {
                "x": 0.0, "y": 0.0, "width": 120.0, "height": 40.0,
                "rotation": 0.0, "flip_x": false, "flip_y": false
            },
            "parent": null, "group": false, "clip_to": null, "mask": null, "adjustment": null,
            "text": {
                "content": "Boxed", "family": "Inter Variable", "size": 12.0,
                "color": [0, 0, 0, 255],
                "bold": false, "italic": false, "underline": false, "strikethrough": false,
                "box": { "width": 120.0 }
            }
        }]);
        write(&manifest, Some(layer_id));
        let loaded = load(&path).unwrap();
        let text_box = loaded.layers[0].text.as_ref().unwrap().r#box.unwrap();
        assert_eq!(text_box.width, 120.0);
        assert_eq!(text_box.min_height, 0.0);

        manifest["version"] = serde_json::json!(11);
        write(&manifest, None);
        assert_eq!(
            load(&path).unwrap_err().to_string(),
            "Unsupported mectov project version"
        );
    }

    #[test]
    fn coloured_letters_write_version_eight_and_plain_text_keeps_its_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("text.mectov");
        let mut renderer = crate::text::TextRenderer::default();
        let mut style = crate::text::TextStyle {
            content: "Coloured letters".into(),
            size: 24.0,
            ..Default::default()
        };
        let mut layer = Layer::image(style.layer_name(), renderer.render(&style).unwrap());
        layer.text = Some(style.clone());
        let mut document = Document::new(320, 240).unwrap();
        document.insert(layer);
        save(&document, &path).unwrap();
        assert_eq!(saved_version(&path), 1, "plain text stays version 1");

        style.set_color_run(0, 8, [220, 20, 20, 255]);
        style.set_color_run(9, 6, [20, 120, 220, 255]);
        let pixels = renderer.render(&style).unwrap();
        let mut layer = Layer::image(style.layer_name(), pixels.clone());
        layer.text = Some(style.clone());
        let mut document = Document::new(320, 240).unwrap();
        document.insert(layer);
        save(&document, &path).unwrap();
        assert_eq!(saved_version(&path), 8);
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.active().unwrap().text, Some(style));
        assert_eq!(
            loaded.active().unwrap().pixels.as_ref().unwrap(),
            &Arc::new(pixels)
        );

        // Clearing the colours drops the document back to the version it had.
        let mut cleared = load(&path).unwrap();
        cleared
            .active_mut()
            .unwrap()
            .text
            .as_mut()
            .unwrap()
            .clear_color_runs();
        save(&cleared, &path).unwrap();
        assert_eq!(saved_version(&path), 1);
    }

    /// A graded camera file has to be version 9: the layer keeps the original
    /// and is developed again on open, so a reader that did not know about the
    /// wheels would show the picture ungraded rather than refuse it.
    #[test]
    fn graded_raw_layers_write_version_nine_and_neutral_wheels_keep_their_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graded.mectov");
        let mut layer = Layer::image("Camera", RgbaImage::new(64, 48));
        layer.raw = Some(crate::raw::RawAsset {
            filename: "camera.nef".into(),
            metadata: crate::raw::RawMetadata {
                width: 64,
                height: 48,
                ..Default::default()
            },
            settings: Default::default(),
            bytes: Arc::new(vec![0; 16]),
        });
        let mut document = Document::new(320, 240).unwrap();
        document.insert(layer);
        save(&document, &path).unwrap();
        assert_eq!(
            saved_version(&path),
            2,
            "an undeveloped camera file is version 2"
        );

        let grading = crate::raw::Grading {
            shadows: crate::raw::GradeWheel {
                hue: 214.0,
                saturation: 47.0,
                luminance: -23.0,
            },
            midtones: crate::raw::GradeWheel {
                hue: 43.0,
                saturation: 31.0,
                luminance: 17.0,
            },
            highlights: crate::raw::GradeWheel {
                hue: 96.0,
                saturation: 39.0,
                luminance: 0.0,
            },
            global: crate::raw::GradeWheel {
                hue: 318.0,
                saturation: 0.0,
                luminance: 0.0,
            },
            blending: 71.0,
            balance: -29.0,
        };
        let mut loaded = load(&path).unwrap();
        loaded
            .active_mut()
            .unwrap()
            .raw
            .as_mut()
            .unwrap()
            .settings
            .grading = grading;
        save(&loaded, &path).unwrap();
        assert_eq!(saved_version(&path), 9);
        let loaded = load(&path).unwrap();
        let settings = &loaded.active().unwrap().raw.as_ref().unwrap().settings;
        assert_eq!(
            settings.grading, grading,
            "the wheels survive the round trip"
        );
        assert_eq!(saved_version(&path), 9, "the wheels alone are version 9");

        // The effects and the calibration that follow them raise it again.
        let mut loaded = loaded;
        {
            let settings = &mut loaded.active_mut().unwrap().raw.as_mut().unwrap().settings;
            settings.glow = 45.0;
            settings.glow_style = crate::raw::GlowStyle::Halation;
            settings.vignette_amount = -30.0;
            settings.vignette_style = crate::raw::VignetteStyle::ColorPriority;
            settings.grain_amount = 20.0;
            settings.calibration.red_saturation = -15.0;
        }
        save(&loaded, &path).unwrap();
        assert_eq!(saved_version(&path), 10);
        let loaded = load(&path).unwrap();
        let settings = &loaded.active().unwrap().raw.as_ref().unwrap().settings;
        assert_eq!(settings.glow, 45.0);
        assert_eq!(settings.glow_style, crate::raw::GlowStyle::Halation);
        assert_eq!(settings.vignette_amount, -30.0);
        assert_eq!(
            settings.vignette_style,
            crate::raw::VignetteStyle::ColorPriority
        );
        assert_eq!(settings.grain_amount, 20.0);
        assert_eq!(settings.calibration.red_saturation, -15.0);
        assert_eq!(settings.grading.blending, 71.0);
        assert_eq!(settings.grading.balance, -29.0);

        // A wheel that only names a hue asks for no change, so the file drops
        // back to the version it had before.
        let mut loaded = loaded;
        loaded
            .active_mut()
            .unwrap()
            .raw
            .as_mut()
            .unwrap()
            .settings
            .grading = crate::raw::Grading {
            global: crate::raw::GradeWheel {
                hue: 12.0,
                ..Default::default()
            },
            ..Default::default()
        };
        {
            // The effects go back to their own defaults as well, since a
            // document keeps the highest version anything in it asks for.
            let settings = &mut loaded.active_mut().unwrap().raw.as_mut().unwrap().settings;
            settings.glow = 0.0;
            settings.vignette_amount = 0.0;
            settings.grain_amount = 0.0;
            settings.calibration = Default::default();
        }
        let settings = &loaded.active().unwrap().raw.as_ref().unwrap().settings;
        assert!(!settings.grading.adjusts() && !settings.adjusts_effects());
        save(&loaded, &path).unwrap();
        assert_eq!(saved_version(&path), 2);
    }

    #[test]
    fn paragraph_boxes_write_version_seven_and_plain_text_keeps_its_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("text.mectov");
        let mut renderer = crate::text::TextRenderer::default();
        let mut style = crate::text::TextStyle {
            content: "Boxed paragraph text".into(),
            size: 24.0,
            ..Default::default()
        };
        let mut layer = Layer::image(style.layer_name(), renderer.render(&style).unwrap());
        layer.text = Some(style.clone());
        let mut document = Document::new(320, 240).unwrap();
        document.insert(layer);
        save(&document, &path).unwrap();
        assert_eq!(saved_version(&path), 1);

        let mut boxed = style.clone();
        boxed.r#box = Some(crate::text::TextBox {
            width: 200.0,
            min_height: 40.0,
            ..Default::default()
        });
        let mut layer = Layer::image(boxed.layer_name(), renderer.render(&boxed).unwrap());
        layer.transform.x = 12.0;
        layer.text = Some(boxed.clone());
        let mut document = Document::new(320, 240).unwrap();
        document.insert(layer);
        document.grid = Some(GridSettings::default());
        save(&document, &path).unwrap();
        assert_eq!(saved_version(&path), 7);
        let mut loaded = load(&path).unwrap();
        assert_eq!(loaded.active().unwrap().text, Some(boxed));
        assert_eq!(
            loaded.active().unwrap().transform.width,
            200.0,
            "the layer is the box, not the ink"
        );

        // Removing the box drops the document back to the version it had before.
        loaded.active_mut().unwrap().text.as_mut().unwrap().r#box = None;
        save(&loaded, &path).unwrap();
        assert_eq!(saved_version(&path), 6);
        style.r#box = None;
        assert!(style.validate().is_ok());
    }

    #[test]
    fn line_shapes_import_as_live_geometry_and_ordinary_shapes_stay_live() {
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
        let shape = document.layers[0].shape.as_ref().unwrap();
        assert_eq!(shape.kind, crate::paint::ShapeKind::Line);
        assert_eq!(shape.line_width, Some(3.0));
        assert_eq!(shape.start, Some(Point::new(0.0, 0.0)));
        assert_eq!(shape.end, Some(Point::new(1.0, 1.0)));
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

    #[test]
    fn live_line_metadata_round_trips_in_the_current_project_format() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("line.mectov");
        let mut document = Document::new(32, 24).unwrap();
        document.insert(
            crate::paint::shape(
                Point::new(4.0, 6.0),
                Point::new(24.0, 16.0),
                crate::paint::ShapeKind::Line,
                [25, 75, 225, 255],
                0.0,
                5.0,
            )
            .unwrap(),
        );
        save(&document, &path).unwrap();
        let mut archive = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let stored: Value =
            serde_json::from_slice(&zip_read(&mut archive, "manifest.json", MAX_MANIFEST).unwrap())
                .unwrap();
        assert_eq!(stored["version"], 5);
        let loaded = load(&path).unwrap();
        let line = loaded.layers.len() - 1;
        assert_eq!(loaded.layers[line].shape, document.layers[line].shape);
    }

    #[test]
    fn existing_shape_metadata_without_line_fields_still_deserializes() {
        let style: crate::document::ShapeStyle = serde_json::from_value(serde_json::json!({
            "kind": "RoundedRectangle",
            "color": [10, 20, 30, 255],
            "corner_radius": 8.0
        }))
        .unwrap();
        assert_eq!(style.kind, crate::paint::ShapeKind::RoundedRectangle);
        assert_eq!(style.line_width, None);
        assert_eq!(style.start, None);
        assert_eq!(style.end, None);
    }
}

#[test]
fn asset_and_image_limits_follow_the_machine() {
    let small = 2 * 1024 * 1024 * 1024;
    // A small machine reads no larger an asset than it has memory for, while a
    // roomy one is held to the gibibyte an image of 200 megapixels can need.
    assert_eq!(max_asset_bytes_for(small), 512 * 1024 * 1024);
    assert_eq!(
        max_asset_bytes_for(64 * 1024 * 1024 * 1024),
        1024 * 1024 * 1024
    );
    assert!(max_asset_bytes() <= MAX_ASSET);
    // Every decoded image passes through validate_size, so an image past this
    // machine's share of memory is refused there, with the ceiling in the
    // message rather than as a silent failure.
    let allowed = crate::document::image_pixels_allowed(memory::total_bytes());
    let side = (allowed as f64).sqrt() as u32 + 1;
    let error = crate::document::validate_size(side, side).unwrap_err();
    assert!(
        error.to_string().contains("megapixels"),
        "the refusal names the ceiling: {error}"
    );
    assert!(crate::document::validate_size(2, 2).is_ok());
}
