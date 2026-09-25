//! Photoshop action descriptors: the key/value tree `TySh` type layers carry.
//!
//! A descriptor is a class ID followed by a flat list of items, each a 4-byte
//! key and a 4-byte `OsType` with its value. The same reader serves the
//! descriptor that starts a type layer and the engine data nested inside it, so
//! every read is bounds checked and nesting is depth limited: a malformed file
//! has to fail rather than spin.

use anyhow::{Context, Result, bail, ensure};

use super::Reader;

/// Nesting limit for descriptors, lists, and object references.
const MAX_DEPTH: usize = 16;
/// A key, class ID, or unit name is at most this long.
const MAX_KEY_BYTES: usize = 4096;
/// Descriptor strings are bounded so one field cannot claim the whole block.
const MAX_STRING_CHARS: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Integer(i64),
    /// A number with its unit, e.g. `#Pxl` for pixels.
    UnitFloat {
        unit: String,
        value: f64,
    },
    String(String),
    Double(f64),
    List(Vec<Value>),
    Dict(Descriptor),
    Enum {
        type_id: String,
        value: String,
    },
    /// A length-prefixed blob whose contents this port does not interpret.
    Glob(Vec<u8>),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Descriptor {
    pub class: String,
    pub items: Vec<(String, Value)>,
}

impl Descriptor {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.items
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub fn string(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::String(value)) => Some(value),
            _ => None,
        }
    }

    pub fn integer(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Some(Value::Integer(value)) => Some(*value),
            _ => None,
        }
    }

    pub fn double(&self, key: &str) -> Option<f64> {
        match self.get(key) {
            Some(Value::Double(value)) => Some(*value),
            Some(Value::UnitFloat { value, .. }) => Some(*value),
            _ => None,
        }
    }

    pub fn dict(&self, key: &str) -> Option<&Descriptor> {
        match self.get(key) {
            Some(Value::Dict(value)) => Some(value),
            _ => None,
        }
    }

    pub fn list(&self, key: &str) -> &[Value] {
        match self.get(key) {
            Some(Value::List(values)) => values,
            _ => &[],
        }
    }

    /// The dictionaries of a list item, which is how fonts, runs, and paragraphs
    /// are stored.
    pub fn dicts(&self, key: &str) -> impl Iterator<Item = &Descriptor> {
        self.list(key).iter().filter_map(|value| match value {
            Value::Dict(dict) => Some(dict),
            _ => None,
        })
    }
}

/// A UTF-16 string. Photoshop prefixes one inside a `GlbO` with a byte count and
/// a literal `TEXT` value with a character count, and getting them backwards
/// doubles or halves the length.
fn read_utf16(reader: &mut Reader<'_>, byte_count: bool) -> Result<String> {
    let count = reader.read_u32()? as usize;
    let (units, length) = if byte_count {
        ensure!(
            count.is_multiple_of(2),
            "Invalid PSD descriptor string length"
        );
        ensure!(
            count <= MAX_STRING_CHARS * 2,
            "PSD descriptor string is too large"
        );
        (count / 2, count)
    } else {
        ensure!(
            count <= MAX_STRING_CHARS,
            "PSD descriptor string is too large"
        );
        (count, count.saturating_mul(2))
    };
    let bytes = reader.take(length)?;
    let text: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect();
    ensure!(units == text.len(), "Invalid PSD descriptor string length");
    let text = String::from_utf16(&text).context("Invalid PSD descriptor string")?;
    Ok(text.trim_end_matches('\0').to_owned())
}

/// A length-prefixed 4-byte class ID. A zero length is the key that means "named
/// after the value's class".
fn read_key(reader: &mut Reader<'_>) -> Result<String> {
    let length = reader.read_u32()? as usize;
    ensure!(length <= MAX_KEY_BYTES, "PSD descriptor key is too large");
    let bytes = reader.take(length)?;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn read_i64(reader: &mut Reader<'_>) -> Result<i64> {
    Ok(i64::from_be_bytes(reader.take(8)?.try_into()?))
}

/// Guard against a count that no remaining byte could possibly satisfy.
fn check_count(count: usize, remaining: usize, per_item: usize, what: &str) -> Result<()> {
    ensure!(
        count <= remaining / per_item + 1,
        "Invalid PSD descriptor {what}"
    );
    Ok(())
}

pub(super) fn read_descriptor_body(reader: &mut Reader<'_>, depth: usize) -> Result<Descriptor> {
    ensure!(depth < MAX_DEPTH, "PSD descriptor nests too deeply");
    let class = read_key(reader)?;
    let count = reader.read_u32()? as usize;
    check_count(count, reader.remaining(), 8, "item count")?;
    let mut items = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let key = read_key(reader)?;
        let os_type = reader.take(4)?;
        let value = read_value(reader, os_type, depth + 1)?;
        items.push((key, value));
    }
    Ok(Descriptor { class, items })
}

fn read_value(reader: &mut Reader<'_>, os_type: &[u8], depth: usize) -> Result<Value> {
    Ok(match os_type {
        // A global object is a version followed by the descriptor it names.
        b"Obj " => Value::Dict(read_descriptor_body(reader, depth)?),
        b"GlbO" => {
            let _version = reader.read_u32()?;
            Value::Dict(read_descriptor_body(reader, depth)?)
        }
        b"VlLs" => {
            ensure!(depth < MAX_DEPTH, "PSD descriptor nests too deeply");
            let count = reader.read_u32()? as usize;
            check_count(count, reader.remaining(), 4, "list length")?;
            let mut values = Vec::with_capacity(count.min(64));
            for _ in 0..count {
                let os_type = reader.take(4)?;
                values.push(read_value(reader, os_type, depth + 1)?);
            }
            Value::List(values)
        }
        // A reference is a count of descriptors, not a byte length.
        b"obj " | b"ObAr" => {
            ensure!(depth < MAX_DEPTH, "PSD descriptor nests too deeply");
            let count = reader.read_u32()? as usize;
            check_count(count, reader.remaining(), 4, "reference length")?;
            let mut values = Vec::with_capacity(count.min(64));
            for _ in 0..count {
                values.push(Value::Dict(read_descriptor_body(reader, depth + 1)?));
            }
            Value::List(values)
        }
        b"enum" => {
            let type_id = read_key(reader)?;
            let value = read_key(reader)?;
            let _enum_type = read_key(reader)?;
            Value::Enum { type_id, value }
        }
        b"long" => Value::Integer(i64::from(reader.read_i32()?)),
        b"Comp" => Value::Integer(read_i64(reader)?),
        b"bool" => Value::Bool(reader.read_u8()? != 0),
        b"dbl " => Value::Double(f64::from_bits(reader.read_u64()?)),
        b"UnFl" => {
            let unit = read_key(reader)?;
            let value = f64::from(f32::from_bits(reader.read_u32()?));
            let _fraction = f32::from_bits(reader.read_u32()?);
            Value::UnitFloat { unit, value }
        }
        b"TEXT" => Value::String(read_utf16(reader, false)?),
        b"ObFl" => {
            // A byte length, then UTF-16 with no second count of its own.
            let length = reader.read_u32()? as usize;
            ensure!(
                length.is_multiple_of(2) && length <= MAX_STRING_CHARS * 2,
                "Invalid PSD descriptor string length"
            );
            let units: Vec<u16> = reader
                .take(length)?
                .chunks_exact(2)
                .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                .collect();
            let text = String::from_utf16(&units).context("Invalid PSD descriptor string")?;
            Value::String(text.trim_end_matches('\0').to_owned())
        }
        b"tdta" | b"alis" | b"Pth " => {
            let length = reader.read_u32()? as usize;
            Value::Glob(reader.take(length)?.to_vec())
        }
        b"type" | b"GlbC" => {
            let length = reader.read_u32()? as usize;
            Value::String(String::from_utf8_lossy(reader.take(length)?).into_owned())
        }
        _ => bail!(
            "Unsupported PSD descriptor type {:?}",
            String::from_utf8_lossy(os_type)
        ),
    })
}

#[cfg(test)]
mod tests {
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

    fn text(value: &str) -> Vec<u8> {
        let units: Vec<u16> = value.encode_utf16().collect();
        let mut out = (units.len() as u32).to_be_bytes().to_vec();
        for unit in units {
            out.extend_from_slice(&unit.to_be_bytes());
        }
        out
    }

    /// A descriptor with an explicit item count, as Photoshop writes it.
    fn descriptor(class: &[u8], items: &[Vec<u8>]) -> Vec<u8> {
        let mut out = key(class);
        out.extend((items.len() as u32).to_be_bytes());
        for item in items {
            out.extend_from_slice(item);
        }
        out
    }

    fn parse(data: &[u8]) -> Result<Descriptor> {
        read_descriptor_body(&mut Reader::new(data), 0)
    }

    #[test]
    fn reads_primitives_keys_and_globs() {
        let data = descriptor(
            b"textLayer",
            &[
                item(b"Nm  ", b"TEXT", &text("Title")),
                item(
                    b"Opct",
                    b"UnFl",
                    &[
                        &key(b"#Prc")[..],
                        &100f32.to_bits().to_be_bytes(),
                        &0f32.to_bits().to_be_bytes(),
                    ]
                    .concat(),
                ),
                item(b"Sz  ", b"dbl ", &48.5f64.to_bits().to_be_bytes()),
                item(b"On  ", b"bool", &[1]),
                item(
                    b"Opn",
                    b"Obj ",
                    &descriptor(b"Ordn", &[item(b"enab", b"bool", &[1])]),
                ),
                item(
                    b"Engn",
                    b"tdta",
                    &[&3u32.to_be_bytes()[..], &[9, 8, 7][..]].concat(),
                ),
                item(
                    b"Just",
                    b"enum",
                    &[
                        &key(b"Ordn")[..],
                        &key(b"centerJustify")[..],
                        &key(b"Justification")[..],
                    ]
                    .concat(),
                ),
            ],
        );
        let parsed = parse(&data).unwrap();
        assert_eq!(parsed.class, "textLayer");
        assert_eq!(parsed.string("Nm  "), Some("Title"));
        assert_eq!(
            parsed.get("Opct"),
            Some(&Value::UnitFloat {
                unit: "#Prc".into(),
                value: 100.0
            })
        );
        assert_eq!(parsed.double("Sz  "), Some(48.5));
        assert_eq!(parsed.get("On  "), Some(&Value::Bool(true)));
        assert_eq!(parsed.dict("Opn").unwrap().class, "Ordn");
        assert_eq!(parsed.get("Engn"), Some(&Value::Glob(vec![9, 8, 7])));
        assert_eq!(
            parsed.get("Just"),
            Some(&Value::Enum {
                type_id: "Ordn".into(),
                value: "centerJustify".into()
            })
        );
    }

    #[test]
    fn reads_lists_of_mixed_values_and_nested_descriptors() {
        let list = [
            2u32.to_be_bytes().to_vec(),
            b"Obj ".to_vec(),
            descriptor(b"Font", &[item(b"Nm  ", b"TEXT", &text("Inter"))]),
            b"long".to_vec(),
            3i32.to_be_bytes().to_vec(),
        ]
        .concat();
        let reference = [1u32.to_be_bytes().to_vec(), descriptor(b"null", &[])].concat();
        let data = descriptor(
            b"null",
            &[
                item(b"Fonts", b"VlLs", &list),
                item(b"Ref  ", b"obj ", &reference),
            ],
        );
        let parsed = parse(&data).unwrap();
        assert_eq!(parsed.list("Fonts").len(), 2);
        let fonts: Vec<_> = parsed.dicts("Fonts").collect();
        assert_eq!(fonts.len(), 1);
        assert_eq!(fonts[0].string("Nm  "), Some("Inter"));
        assert_eq!(parsed.dict("Ref  "), None);
        assert_eq!(parsed.list("Ref  ").len(), 1);
    }

    #[test]
    fn rejects_truncated_deep_and_unknown_data() {
        let truncated = descriptor(b"null", &[item(b"bad", b"Obj ", &[0, 0, 0, 1, 0, 0])]);
        let error = parse(&truncated).unwrap_err().to_string();
        assert!(error.contains("Unexpected end of PSD data"), "{error}");

        let mut deep = descriptor(b"null", &[]);
        for _ in 0..MAX_DEPTH + 2 {
            deep = descriptor(b"null", &[item(b"n", b"Obj ", &deep)]);
        }
        let error = parse(&deep).unwrap_err().to_string();
        assert!(error.contains("nests too deeply"), "{error}");

        let unknown = descriptor(b"null", &[item(b"weird", b"zzzz", &[1, 2, 3, 4])]);
        let error = parse(&unknown).unwrap_err().to_string();
        assert!(error.contains("Unsupported PSD descriptor type"), "{error}");

        let lying_count = b"\0\0\0\x01\0\0\0\x09Obj null".to_vec();
        assert!(
            parse(&lying_count).is_err(),
            "an impossible count is rejected"
        );
    }

    #[test]
    fn reads_a_large_glob_and_a_file_path() {
        let path = {
            let units: Vec<u16> = "/tmp/a.psd".encode_utf16().collect();
            let mut data = (units.len() as u32 * 2).to_be_bytes().to_vec();
            for unit in units {
                data.extend_from_slice(&unit.to_be_bytes());
            }
            data
        };
        let data = descriptor(
            b"null",
            &[
                item(
                    b"big ",
                    b"tdta",
                    &[&4096u32.to_be_bytes()[..], &vec![7u8; 4096][..]].concat(),
                ),
                item(b"path", b"ObFl", &path),
            ],
        );
        let parsed = parse(&data).unwrap();
        assert_eq!(
            parsed.get("big "),
            Some(&Value::Glob(vec![7u8; 4096])),
            "a large blob survives"
        );
        assert_eq!(parsed.string("path"), Some("/tmp/a.psd"));
    }
}
