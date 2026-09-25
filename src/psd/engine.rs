//! The engine data a Photoshop type layer keeps its text in.
//!
//! `TySh` does not hold characters. Its text descriptor points at engine data,
//! which carries the document's font set and one array of styled runs. This
//! reader keeps the run boundaries, because a run carries the font, size, and
//! colour that apply to it, but reports a single style: the first run's.
//!
//! Where exactly Photoshop nests the run array has moved between versions, so
//! the tree is searched within a depth limit instead of assuming a path.

use anyhow::{Context, Result, ensure};

use super::descriptor::{Descriptor, Value};

/// How deep the search for the run array and the font set may go.
const MAX_SEARCH_DEPTH: usize = 6;
/// A run of characters, bounded so a bad length cannot be trusted.
const MAX_TEXT_CHARS: usize = 1 << 18;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    /// Characters in this run, exactly as Photoshop stored them.
    pub text: String,
    /// Index into the document's font list, when the run names one.
    pub font: Option<i64>,
    /// Stroke weight in Photoshop's `Sz` unit, which is points.
    pub size: Option<f64>,
    /// Fill colour, 0–255, when the run carries one.
    pub color: Option<[u8; 4]>,
}

/// The font name, size, and fill colour a single text layer can carry.
pub type PrimaryStyle<'a> = (Option<&'a str>, Option<f64>, Option<[u8; 4]>);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineText {
    /// Every run's characters, joined in order.
    pub text: String,
    pub runs: Vec<Run>,
    /// The document's font set, in the order Photoshop stores it.
    pub fonts: Vec<Font>,
}

impl EngineText {
    /// The style mectov can carry in a single text layer: the first run's.
    pub fn primary_style(&self) -> Option<PrimaryStyle<'_>> {
        let first = self.runs.iter().find(|run| !run.text.is_empty())?;
        let entry = first
            .font
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| self.fonts.get(index));
        let font = entry
            .map(|entry| entry.name.as_str())
            .filter(|name| !name.trim().is_empty());
        // A run that names no size leaves the font set's size in charge.
        let size = first.size.or_else(|| entry.and_then(|entry| entry.size));
        Some((font, size, first.color))
    }
}

/// Read the text out of a type layer's descriptor.
pub fn parse(text_descriptor: &Descriptor) -> Result<EngineText> {
    let run_array =
        find(text_descriptor, 0, "RunArray").context("PSD type layer has no run array")?;
    let fonts = find(text_descriptor, 0, "DocumentResources")
        .and_then(fonts)
        .unwrap_or_default();
    let mut runs = Vec::new();
    // Photoshop stores one run array here; some versions wrap several in a list.
    match run_array.get("RunArray") {
        Some(Value::Dict(array)) => runs.extend(merge_runs(array)?),
        Some(Value::List(arrays)) => {
            for array in arrays {
                if let Value::Dict(array) = array {
                    runs.extend(merge_runs(array)?);
                }
            }
        }
        _ => {}
    }
    ensure!(
        runs.iter().any(|run| !run.text.is_empty()),
        "PSD type layer has no text"
    );
    let text = runs.iter().map(|run| run.text.as_str()).collect();
    Ok(EngineText { text, runs, fonts })
}

/// The first descriptor in the tree that carries `key`.
fn find<'a>(descriptor: &'a Descriptor, depth: usize, key: &str) -> Option<&'a Descriptor> {
    if descriptor.get(key).is_some() {
        return Some(descriptor);
    }
    if depth >= MAX_SEARCH_DEPTH {
        return None;
    }
    for (_, value) in &descriptor.items {
        match value {
            Value::Dict(child) => {
                if let Some(found) = find(child, depth + 1, key) {
                    return Some(found);
                }
            }
            Value::List(values) => {
                for value in values {
                    if let Value::Dict(child) = value
                        && let Some(found) = find(child, depth + 1, key)
                    {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// A font as the document's font set lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Font {
    /// The family name, which is all mectov can select.
    pub name: String,
    /// The size the font set offers it at, when it names one.
    pub size: Option<f64>,
}

/// `ResourceDict` → `FontSet` → a list of fonts, each with a `Name`.
fn fonts(resources: &Descriptor) -> Option<Vec<Font>> {
    resources
        .dict("DocumentResources")
        .and_then(|resource| resource.dict("ResourceDict"))
        .and_then(|dict| dict.dict("FontSet"))
        .map(|set| {
            set.dicts("Font")
                .map(|font| Font {
                    name: font.string("Name").unwrap_or_default().to_owned(),
                    size: point_size(font),
                })
                .collect()
        })
}

/// Photoshop writes a text size as a double in points. Some versions store the
/// run's size as a 16.16 fixed-point long instead, the encoding the layer
/// transform and the adjustment values use, so the descriptor's own type
/// decides which one this is.
fn point_size(style: &Descriptor) -> Option<f64> {
    if let Some(size) = style.double("Sz  ").or_else(|| style.double("Sz")) {
        return Some(size);
    }
    style
        .integer("Sz  ")
        .or_else(|| style.integer("Sz"))
        .map(|fixed| fixed as f64 / 65536.0)
        .filter(|size| size.is_finite())
}

/// One run array: its lengths, its styles, and its characters, zipped by index.
fn merge_runs(run_array: &Descriptor) -> Result<Vec<Run>> {
    let lengths = run_lengths(run_array);
    let styles = run_styles(run_array);
    let texts = run_texts(run_array);
    let count = lengths.len().max(texts.len());
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut merged = Vec::with_capacity(count);
    for index in 0..count {
        // Photoshop ends a line with a carriage return; mectov's text is
        // newline-separated, so the one is translated as it is read.
        let text = texts
            .get(index)
            .map(|text| text.replace('\r', "\n"))
            .unwrap_or_default();
        ensure!(
            text.chars().count() <= MAX_TEXT_CHARS,
            "PSD text run is too long"
        );
        merged.push(Run {
            text,
            ..styles.get(index).cloned().unwrap_or_default()
        });
    }
    Ok(merged)
}

/// `RunLengthArray` holds a list of character counts. They are read for their
/// presence, which is how Photoshop marks a run array as populated, but mectov
/// joins the run text instead of splitting one string.
fn run_lengths(run_array: &Descriptor) -> Vec<usize> {
    let values = match run_array.get("RunLengthArray") {
        Some(Value::List(values)) => values.clone(),
        Some(Value::Dict(inner)) => inner.list("RunLengthArray").to_vec(),
        _ => Vec::new(),
    };
    values
        .iter()
        .filter_map(|value| match value {
            Value::Integer(length) => usize::try_from(*length).ok(),
            _ => None,
        })
        .collect()
}

/// `StyleRunArray` → `RunArray` → text runs, each naming a font and a colour.
fn run_styles(run_array: &Descriptor) -> Vec<Run> {
    dicts(run_array, "StyleRunArray")
        .into_iter()
        .map(|run| {
            // The run's `Style` item is itself the font style; some versions
            // wrap it once more.
            let style = run.dict("Style");
            let font_style = style.and_then(|style| style.dict("FontStyle")).or(style);
            Run {
                text: String::new(),
                font: font_style.and_then(|style| style.integer("Font")),
                size: font_style.and_then(point_size),
                color: font_style.and_then(fill_color),
            }
        })
        .collect()
}

/// `FillColor` is a class whose colour children are named `Rd `, `Grn `, and
/// `Bl  `, each holding 0–255.
fn fill_color(style: &Descriptor) -> Option<[u8; 4]> {
    let fill = style.dict("FillColor")?;
    let channel = |holder: &Descriptor, name: &str| {
        holder
            .integer(name)
            .map(|value| value as f64)
            .or_else(|| holder.double(name))
    };
    // Components sit directly on the colour, or one level down behind their own
    // key, depending on the version.
    let component = |name: &str| {
        channel(fill, name)
            .or_else(|| fill.dict(name).and_then(|nested| channel(nested, name)))
            .map(|value| value.clamp(0.0, 255.0) as u8)
    };
    Some([
        component("Rd  ")?,
        component("Grn ")?,
        component("Bl  ")?,
        255,
    ])
}

/// `RunTextArray` holds the characters themselves, one descriptor per run.
fn run_texts(run_array: &Descriptor) -> Vec<String> {
    dicts(run_array, "RunTextArray")
        .into_iter()
        .map(|run| run.string("RunText").unwrap_or_default().to_owned())
        .collect()
}

/// The dictionaries under `key`, whether Photoshop wrote them as the list
/// itself or wrapped in a descriptor that holds one.
fn dicts<'a>(descriptor: &'a Descriptor, key: &str) -> Vec<&'a Descriptor> {
    fn collect(values: &[Value]) -> Vec<&Descriptor> {
        values
            .iter()
            .filter_map(|value| match value {
                Value::Dict(dict) => Some(dict),
                _ => None,
            })
            .collect()
    }
    match descriptor.get(key) {
        Some(Value::List(values)) => collect(values),
        Some(Value::Dict(inner)) => match inner.get("RunArray") {
            Some(Value::List(values)) => collect(values),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::Reader;
    use super::super::descriptor::read_descriptor_body as read;
    use super::*;

    fn key(name: &[u8]) -> Vec<u8> {
        let mut out = (name.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(name);
        out
    }

    fn item(key_name: &[u8], os_type: &[u8], value: &[u8]) -> Vec<u8> {
        let mut out = key(key_name);
        out.extend_from_slice(os_type);
        out.extend_from_slice(value);
        out
    }

    fn dict_value(class: &[u8], items: &[Vec<u8>]) -> Vec<u8> {
        let mut out = key(class);
        out.extend((items.len() as u32).to_be_bytes());
        for item in items {
            out.extend_from_slice(item);
        }
        out
    }

    /// A list of descriptors: the count, then each element with its `Obj ` type.
    fn dict_list(items: &[Vec<u8>]) -> Vec<u8> {
        let mut out = (items.len() as u32).to_be_bytes().to_vec();
        for item in items {
            out.extend_from_slice(b"Obj ");
            out.extend_from_slice(item.as_slice());
        }
        out
    }

    /// A list of typed values, where each item already carries its `OsType`.
    fn typed_list(items: &[Vec<u8>]) -> Vec<u8> {
        let mut out = (items.len() as u32).to_be_bytes().to_vec();
        for item in items {
            out.extend_from_slice(item.as_slice());
        }
        out
    }

    fn text_value(value: &str) -> Vec<u8> {
        let units: Vec<u16> = value.encode_utf16().collect();
        let mut out = (units.len() as u32).to_be_bytes().to_vec();
        for unit in units {
            out.extend_from_slice(&unit.to_be_bytes());
        }
        out
    }

    fn long(value: i32) -> Vec<u8> {
        let mut out = b"long".to_vec();
        out.extend(value.to_be_bytes());
        out
    }

    fn font_set(names: &[&str]) -> Vec<u8> {
        let fonts = names
            .iter()
            .map(|name| {
                dict_value(
                    b"Font",
                    &[
                        item(b"Name", b"TEXT", &text_value(name)),
                        item(b"Sz  ", b"dbl ", &24f64.to_bits().to_be_bytes()),
                    ],
                )
            })
            .collect::<Vec<_>>();
        item(
            b"FontSet",
            b"Obj ",
            &dict_value(b"FontSet", &[item(b"Font", b"VlLs", &dict_list(&fonts))]),
        )
    }

    /// How a run's style spells its size, because Photoshop uses all three.
    #[derive(Clone)]
    enum Size {
        Points(f64),
        Fixed(i32),
        Absent,
    }

    fn style_run(font: i32, size: Size, rgb: [u8; 3]) -> Vec<u8> {
        let colour = dict_value(
            b"RGBColor",
            &[
                item(b"Rd  ", b"long", &(rgb[0] as i32).to_be_bytes()),
                item(b"Grn ", b"long", &(rgb[1] as i32).to_be_bytes()),
                item(b"Bl  ", b"long", &(rgb[2] as i32).to_be_bytes()),
            ],
        );
        let mut items = vec![item(b"Font", b"long", &font.to_be_bytes())];
        match size {
            Size::Points(points) => {
                items.push(item(b"Sz  ", b"dbl ", &points.to_bits().to_be_bytes()));
            }
            Size::Fixed(fixed) => {
                items.push(item(b"Sz  ", b"long", &fixed.to_be_bytes()));
            }
            Size::Absent => {}
        }
        items.push(item(b"FillColor", b"Obj ", &colour));
        let style = dict_value(b"FontStyle", &items);
        dict_value(
            b"TextRun",
            &[
                item(b"RunLength", b"long", &1i32.to_be_bytes()),
                item(b"Style", b"Obj ", &style),
            ],
        )
    }

    fn run_array(lengths: &[i32], styles: &[(i32, Size, [u8; 3])], texts: &[&str]) -> Vec<u8> {
        let length_values = lengths.iter().copied().map(long).collect::<Vec<_>>();
        let style_values = styles
            .iter()
            .map(|(font, size, rgb)| style_run(*font, size.clone(), *rgb))
            .collect::<Vec<_>>();
        let text_values = texts
            .iter()
            .map(|value| dict_value(b"TextRun", &[item(b"RunText", b"TEXT", &text_value(value))]))
            .collect::<Vec<_>>();
        dict_value(
            b"RunArrayCore",
            &[
                item(b"RunLengthArray", b"VlLs", &typed_list(&length_values)),
                item(
                    b"StyleRunArray",
                    b"Obj ",
                    &dict_value(
                        b"RunArrayCore",
                        &[item(b"RunArray", b"VlLs", &dict_list(&style_values))],
                    ),
                ),
                item(
                    b"RunTextArray",
                    b"Obj ",
                    &dict_value(
                        b"RunArrayCore",
                        &[item(b"RunArray", b"VlLs", &dict_list(&text_values))],
                    ),
                ),
            ],
        )
    }

    /// A type layer's text descriptor: resources and a run array in one `GlbO`,
    /// which is the shape Photoshop writes.
    fn text_descriptor(fonts: &[&str], runs: Vec<u8>) -> Vec<u8> {
        let resources = item(
            b"DocumentResources",
            b"Obj ",
            &dict_value(
                b"DocumentResources",
                &[item(
                    b"ResourceDict",
                    b"Obj ",
                    &dict_value(b"ResourceDict", &[font_set(fonts)]),
                )],
            ),
        );
        let mut engine_items = vec![resources];
        if !runs.is_empty() {
            engine_items.push(item(b"RunArray", b"Obj ", &runs));
        }
        let engine = item(
            b"EngineData",
            b"GlbO",
            &[
                4u32.to_be_bytes().to_vec(),
                dict_value(b"EngineDataCore", &engine_items),
            ]
            .concat(),
        );
        dict_value(b"textLayer", &[engine])
    }

    fn parse_bytes(data: &[u8]) -> Result<EngineText> {
        let descriptor = read(&mut Reader::new(data), 0)?;
        parse(&descriptor)
    }

    #[test]
    fn reads_two_runs_and_reports_the_first_style() {
        let data = text_descriptor(
            &["Helvetica", "Georgia"],
            run_array(
                &[5, 6],
                &[
                    (0, Size::Points(18.0), [255, 0, 0]),
                    (1, Size::Points(24.0), [0, 128, 255]),
                ],
                &["Hello", " world"],
            ),
        );
        let parsed = parse_bytes(&data).unwrap();
        assert_eq!(parsed.text, "Hello world");
        assert_eq!(
            parsed
                .fonts
                .iter()
                .map(|font| font.name.as_str())
                .collect::<Vec<_>>(),
            ["Helvetica", "Georgia"]
        );
        assert_eq!(parsed.runs.len(), 2);
        assert_eq!(parsed.runs[1].text, " world");
        assert_eq!(parsed.runs[1].font, Some(1));
        assert_eq!(parsed.runs[1].size, Some(24.0));
        assert_eq!(parsed.runs[1].color, Some([0, 128, 255, 255]));
        assert_eq!(
            parsed.primary_style(),
            Some((Some("Helvetica"), Some(18.0), Some([255, 0, 0, 255])))
        );
    }

    #[test]
    fn an_empty_or_unreadable_run_array_fails_instead_of_inventing_text() {
        let empty = text_descriptor(&["Inter"], Vec::new());
        assert!(parse_bytes(&empty).is_err());

        let truncated = text_descriptor(
            &["Inter"],
            run_array(&[2], &[(0, Size::Points(12.0), [10, 20, 30])], &["hi"]),
        );
        for length in [0, 8, 32, truncated.len() - 1] {
            assert!(parse_bytes(&truncated[..length]).is_err(), "{length}");
        }
    }

    #[test]
    fn a_carriage_return_becomes_a_newline() {
        let data = text_descriptor(
            &["Inter"],
            run_array(
                &[11],
                &[(0, Size::Points(12.0), [1, 2, 3])],
                &["one\rtwo\rthree"],
            ),
        );
        let parsed = parse_bytes(&data).unwrap();
        assert_eq!(parsed.text, "one\ntwo\nthree");
    }

    #[test]
    fn a_fixed_point_size_is_read_as_points() {
        // 14.5 points in the 16.16 encoding Photoshop uses for some runs.
        let fixed = (14.5 * 65536.0) as i32;
        let data = text_descriptor(
            &["Inter"],
            run_array(&[4], &[(0, Size::Fixed(fixed), [1, 2, 3])], &["Test"]),
        );
        assert_eq!(
            parse_bytes(&data).unwrap().primary_style().unwrap().1,
            Some(14.5)
        );
    }

    #[test]
    fn a_run_without_a_size_falls_back_to_the_font_set() {
        let data = text_descriptor(
            &["Inter"],
            run_array(&[4], &[(0, Size::Absent, [1, 2, 3])], &["Test"]),
        );
        let parsed = parse_bytes(&data).unwrap();
        // The font set offers Inter at 24 points, so that is the size in force.
        assert_eq!(
            parsed.primary_style(),
            Some((Some("Inter"), Some(24.0), Some([1, 2, 3, 255])))
        );
    }

    #[test]
    fn a_layer_without_a_font_set_still_imports_its_characters() {
        let runs = run_array(&[4], &[(7, Size::Points(12.0), [1, 2, 3])], &["Test"]);
        let data = dict_value(b"textLayer", &[item(b"RunArray", b"Obj ", &runs)]);
        let parsed = parse_bytes(&data).unwrap();
        assert_eq!(parsed.text, "Test");
        assert!(parsed.fonts.is_empty());
        // The font index cannot be resolved, so no name is claimed.
        assert_eq!(parsed.primary_style().unwrap().0, None);
    }
}
