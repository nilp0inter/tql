//! Tracker `manifest.toml` parser (DESIGN.md §12).
//!
//! Pure: takes TOML bytes (or a path), produces a validated [`Manifest`].
//! No Rhai, no I/O beyond reading the manifest file itself.
//!
//! The manifest is the single source of truth for a tracker's metadata and
//! input schema. Downstream legs consume [`Manifest`] to:
//!   * generate the JSON Schema handed to MCP/REST,
//!   * build dynamic clap subcommands,
//!   * marshal validated input into the Rhai engine.

use std::fs;
use std::path::Path;

use serde::Deserialize;

/// Soft cap on the length of a tracker `name` (matches §10's category cap).
pub const MAX_TRACKER_NAME_BYTES: usize = 128;

/// A parsed, validated tracker manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub canonical_category: String,
    /// `None` when absent; the host warns on load when missing per §12.
    pub version: Option<String>,
    pub description: String,
    pub url_pattern: Option<String>,
    pub inputs: Vec<InputField>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputField {
    pub name: String,
    pub field_type: FieldType,
    pub required: bool,
    pub description: String,
    pub default: Option<toml::Value>,
    pub cli_flag: Option<String>,
    pub cli_separator: Option<String>,
    pub min_items: Option<usize>,
    pub max_items: Option<usize>,
}

/// One of the manifest field types from §12.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldType {
    String,
    Int,
    Bool,
    Array(Box<FieldType>),
    Enum(Vec<String>),
    MapStringString,
}

#[derive(Debug)]
pub enum ManifestError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Validation(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Io(e) => write!(f, "manifest io error: {e}"),
            ManifestError::Parse(e) => write!(f, "manifest parse error: {e}"),
            ManifestError::Validation(m) => write!(f, "manifest validation error: {m}"),
        }
    }
}

impl std::error::Error for ManifestError {}

impl From<std::io::Error> for ManifestError {
    fn from(e: std::io::Error) -> Self {
        ManifestError::Io(e)
    }
}

impl From<toml::de::Error> for ManifestError {
    fn from(e: toml::de::Error) -> Self {
        ManifestError::Parse(e)
    }
}

// ---------------------------------------------------------------------------
// Wire (TOML) types — kept private so the public surface stays clean.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct WireManifest {
    name: String,
    canonical_category: String,
    #[serde(default)]
    version: Option<String>,
    description: String,
    #[serde(default)]
    url_pattern: Option<String>,
    #[serde(default, rename = "input")]
    inputs: Vec<WireInputField>,
}

#[derive(Deserialize)]
struct WireInputField {
    name: String,
    #[serde(rename = "type")]
    type_: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    description: String,
    #[serde(default)]
    default: Option<toml::Value>,
    #[serde(default)]
    cli_flag: Option<String>,
    #[serde(default)]
    cli_separator: Option<String>,
    #[serde(default)]
    min_items: Option<usize>,
    #[serde(default)]
    max_items: Option<usize>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Read a manifest from disk and parse it.
pub fn load(path: &Path) -> Result<Manifest, ManifestError> {
    let raw = fs::read_to_string(path)?;
    parse(&raw)
}

/// Parse a manifest from a TOML string.
pub fn parse(raw: &str) -> Result<Manifest, ManifestError> {
    let wire: WireManifest = toml::from_str(raw)?;
    validate(wire)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate(w: WireManifest) -> Result<Manifest, ManifestError> {
    if w.name.is_empty() {
        return Err(ManifestError::Validation("`name` must not be empty".into()));
    }
    if w.name.len() > MAX_TRACKER_NAME_BYTES {
        return Err(ManifestError::Validation(format!(
            "`name` exceeds {MAX_TRACKER_NAME_BYTES} bytes"
        )));
    }
    if !is_valid_tracker_name(&w.name) {
        return Err(ManifestError::Validation(format!(
            "`name` must match [A-Za-z0-9._-]+ (got {:?})",
            w.name
        )));
    }
    if w.canonical_category.is_empty() {
        return Err(ManifestError::Validation(
            "`canonical_category` must not be empty".into(),
        ));
    }
    if w.description.trim().is_empty() {
        return Err(ManifestError::Validation(
            "`description` must not be empty".into(),
        ));
    }

    let mut inputs = Vec::with_capacity(w.inputs.len());
    let mut seen = std::collections::HashSet::new();
    for f in w.inputs {
        if !seen.insert(f.name.clone()) {
            return Err(ManifestError::Validation(format!(
                "duplicate input field name {:?}",
                f.name
            )));
        }
        inputs.push(validate_input(f)?);
    }

    Ok(Manifest {
        name: w.name,
        canonical_category: w.canonical_category,
        version: w.version,
        description: w.description,
        url_pattern: w.url_pattern,
        inputs,
    })
}

fn validate_input(f: WireInputField) -> Result<InputField, ManifestError> {
    if f.name.is_empty() {
        return Err(ManifestError::Validation(
            "input `name` must not be empty".into(),
        ));
    }
    if !is_valid_field_name(&f.name) {
        return Err(ManifestError::Validation(format!(
            "input name must match [A-Za-z_][A-Za-z0-9_]* (got {:?})",
            f.name
        )));
    }
    let field_type = parse_field_type(&f.type_).map_err(|m| {
        ManifestError::Validation(format!("input {:?}: invalid type: {m}", f.name))
    })?;

    let is_array = matches!(field_type, FieldType::Array(_));
    if !is_array && (f.min_items.is_some() || f.max_items.is_some()) {
        return Err(ManifestError::Validation(format!(
            "input {:?}: min_items/max_items only apply to array types",
            f.name
        )));
    }
    if let (Some(lo), Some(hi)) = (f.min_items, f.max_items) {
        if lo > hi {
            return Err(ManifestError::Validation(format!(
                "input {:?}: min_items ({lo}) > max_items ({hi})",
                f.name
            )));
        }
    }

    if let Some(ref d) = f.default {
        type_check_default(&field_type, d).map_err(|m| {
            ManifestError::Validation(format!("input {:?}: default {m}", f.name))
        })?;
    }

    if !is_array && f.cli_separator.is_some() {
        return Err(ManifestError::Validation(format!(
            "input {:?}: cli_separator only applies to array types",
            f.name
        )));
    }

    Ok(InputField {
        name: f.name,
        field_type,
        required: f.required,
        description: f.description,
        default: f.default,
        cli_flag: f.cli_flag,
        cli_separator: f.cli_separator,
        min_items: f.min_items,
        max_items: f.max_items,
    })
}

/// Parse a `type = "..."` declaration. Recursive for `array<...>`.
fn parse_field_type(s: &str) -> Result<FieldType, String> {
    let s = s.trim();
    match s {
        "string" => Ok(FieldType::String),
        "int" => Ok(FieldType::Int),
        "bool" => Ok(FieldType::Bool),
        "map<string,string>" | "map<string, string>" => Ok(FieldType::MapStringString),
        _ => {
            if let Some(inner) = strip_generic(s, "array") {
                let inner_ty = parse_field_type(inner)?;
                Ok(FieldType::Array(Box::new(inner_ty)))
            } else if let Some(inner) = strip_generic(s, "enum") {
                let variants: Vec<String> = inner
                    .split(',')
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
                    .collect();
                if variants.is_empty() {
                    return Err(format!("enum<...> requires at least one variant ({s:?})"));
                }
                Ok(FieldType::Enum(variants))
            } else {
                Err(format!("unknown type {s:?}"))
            }
        }
    }
}

/// Returns `Some(inner)` if `s` matches `<head><...>`. Pure string slicing,
/// so nested generics like `array<array<string>>` parse correctly via
/// recursion in [`parse_field_type`].
fn strip_generic<'a>(s: &'a str, head: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(head)?;
    let rest = rest.strip_prefix('<')?;
    let inner = rest.strip_suffix('>')?;
    Some(inner)
}

fn type_check_default(ty: &FieldType, v: &toml::Value) -> Result<(), String> {
    let ok = match (ty, v) {
        (FieldType::String, toml::Value::String(_)) => true,
        (FieldType::Int, toml::Value::Integer(_)) => true,
        (FieldType::Bool, toml::Value::Boolean(_)) => true,
        (FieldType::Array(inner), toml::Value::Array(items)) => {
            for it in items {
                type_check_default(inner, it)?;
            }
            true
        }
        (FieldType::Enum(variants), toml::Value::String(s)) => variants.iter().any(|v| v == s),
        (FieldType::MapStringString, toml::Value::Table(t)) => {
            t.values().all(|v| matches!(v, toml::Value::String(_)))
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("does not match declared type ({ty:?})"))
    }
}

fn is_valid_tracker_name(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn is_valid_field_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return false };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn mam_manifest() -> &'static str {
        r#"
name = "myanonamouse"
canonical_category = "myanonamouse.net"
version = "1.0.0"
description = "Add a torrent from MyAnonamouse to qBittorrent."

url_pattern = '^https?://(www\.)?myanonamouse\.net/t/\d+'

[[input]]
name = "url"
type = "string"
required = true
description = "Tracker URL of the torrent page (for logging only)."

[[input]]
name = "categories"
type = "array<string>"
required = true
min_items = 1
description = "All categories the torrent is filed under."
cli_flag = "--categories"

[[input]]
name = "author"
type = "string"
required = false
description = "Author name as displayed on the torrent page."

[[input]]
name = "labels"
type = "array<string>"
required = false
default = []
description = "Free-form labels."
cli_flag = "--label"
"#
    }

    #[test]
    fn parses_full_mam_example() {
        let m = parse(mam_manifest()).unwrap();
        assert_eq!(m.name, "myanonamouse");
        assert_eq!(m.canonical_category, "myanonamouse.net");
        assert_eq!(m.version.as_deref(), Some("1.0.0"));
        assert!(m.url_pattern.is_some());
        assert_eq!(m.inputs.len(), 4);

        let url = &m.inputs[0];
        assert_eq!(url.name, "url");
        assert_eq!(url.field_type, FieldType::String);
        assert!(url.required);

        let cats = &m.inputs[1];
        assert_eq!(
            cats.field_type,
            FieldType::Array(Box::new(FieldType::String))
        );
        assert_eq!(cats.min_items, Some(1));
        assert_eq!(cats.cli_flag.as_deref(), Some("--categories"));

        let labels = &m.inputs[3];
        assert!(matches!(labels.default, Some(toml::Value::Array(_))));
    }

    #[test]
    fn missing_name_rejected() {
        let raw = r#"
canonical_category = "x"
description = "y"
"#;
        let err = parse(raw).unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
    }

    #[test]
    fn empty_description_rejected() {
        let raw = r#"
name = "t"
canonical_category = "x"
description = "   "
"#;
        let err = parse(raw).unwrap_err();
        assert!(matches!(err, ManifestError::Validation(_)));
    }

    #[test]
    fn duplicate_input_names_rejected() {
        let raw = r#"
name = "t"
canonical_category = "x"
description = "y"

[[input]]
name = "a"
type = "string"

[[input]]
name = "a"
type = "int"
"#;
        let err = parse(raw).unwrap_err();
        assert!(format!("{err}").contains("duplicate"));
    }

    #[test]
    fn unknown_type_rejected() {
        let raw = r#"
name = "t"
canonical_category = "x"
description = "y"

[[input]]
name = "a"
type = "float"
"#;
        let err = parse(raw).unwrap_err();
        assert!(format!("{err}").contains("unknown type"));
    }

    #[test]
    fn nested_array_parses() {
        assert_eq!(
            parse_field_type("array<array<string>>").unwrap(),
            FieldType::Array(Box::new(FieldType::Array(Box::new(FieldType::String))))
        );
    }

    #[test]
    fn enum_parses_and_default_must_be_variant() {
        let raw = r#"
name = "t"
canonical_category = "x"
description = "y"

[[input]]
name = "color"
type = "enum<red, green, blue>"
default = "purple"
"#;
        let err = parse(raw).unwrap_err();
        assert!(format!("{err}").contains("does not match declared type"));

        let raw_ok = r#"
name = "t"
canonical_category = "x"
description = "y"

[[input]]
name = "color"
type = "enum<red, green, blue>"
default = "green"
"#;
        let m = parse(raw_ok).unwrap();
        assert_eq!(
            m.inputs[0].field_type,
            FieldType::Enum(vec!["red".into(), "green".into(), "blue".into()])
        );
    }

    #[test]
    fn map_string_string_parses() {
        let raw = r#"
name = "t"
canonical_category = "x"
description = "y"

[[input]]
name = "labels"
type = "map<string, string>"
[input.default]
foo = "bar"
"#;
        let m = parse(raw).unwrap();
        assert_eq!(m.inputs[0].field_type, FieldType::MapStringString);
    }

    #[test]
    fn min_items_on_non_array_rejected() {
        let raw = r#"
name = "t"
canonical_category = "x"
description = "y"

[[input]]
name = "a"
type = "string"
min_items = 1
"#;
        let err = parse(raw).unwrap_err();
        assert!(format!("{err}").contains("min_items/max_items"));
    }

    #[test]
    fn min_greater_than_max_rejected() {
        let raw = r#"
name = "t"
canonical_category = "x"
description = "y"

[[input]]
name = "a"
type = "array<string>"
min_items = 5
max_items = 2
"#;
        let err = parse(raw).unwrap_err();
        assert!(format!("{err}").contains("min_items"));
    }

    #[test]
    fn cli_separator_on_non_array_rejected() {
        let raw = r#"
name = "t"
canonical_category = "x"
description = "y"

[[input]]
name = "a"
type = "string"
cli_separator = ","
"#;
        let err = parse(raw).unwrap_err();
        assert!(format!("{err}").contains("cli_separator"));
    }

    #[test]
    fn default_type_mismatch_rejected() {
        let raw = r#"
name = "t"
canonical_category = "x"
description = "y"

[[input]]
name = "a"
type = "int"
default = "hello"
"#;
        let err = parse(raw).unwrap_err();
        assert!(format!("{err}").contains("does not match declared type"));
    }

    #[test]
    fn invalid_tracker_name_rejected() {
        let raw = r#"
name = "has spaces"
canonical_category = "x"
description = "y"
"#;
        let err = parse(raw).unwrap_err();
        assert!(format!("{err}").contains("[A-Za-z0-9._-]"));
    }

    #[test]
    fn invalid_field_name_rejected() {
        let raw = r#"
name = "t"
canonical_category = "x"
description = "y"

[[input]]
name = "1bad"
type = "string"
"#;
        let err = parse(raw).unwrap_err();
        assert!(format!("{err}").contains("[A-Za-z_]"));
    }

    #[test]
    fn version_optional() {
        let raw = r#"
name = "t"
canonical_category = "x"
description = "y"
"#;
        let m = parse(raw).unwrap();
        assert!(m.version.is_none());
        assert!(m.inputs.is_empty());
    }
}
