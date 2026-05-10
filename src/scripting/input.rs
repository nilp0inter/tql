//! Marshal manifest-typed inputs into a Rhai `Map` (Leg 8b).
//!
//! Transports (REST, MCP, CLI) all converge on a JSON-shaped per-tracker
//! input. This module takes a parsed [`Manifest`] and a `serde_json::Value`,
//! validates the value against the manifest's input schema, and produces a
//! `rhai::Map` ready to hand to [`super::host::run_classify`].
//!
//! Validation enforces:
//!   * type matches `FieldType`,
//!   * required fields are present (or have a default),
//!   * `min_items`/`max_items` for arrays,
//!   * enum values are one of the declared variants,
//!   * `map<string,string>` values are all strings,
//!   * unknown top-level fields are rejected (catches caller typos).
//!
//! Unset optional fields with no default are *omitted* from the output map
//! (the script sees `input.foo == ()` per Rhai's "missing key" semantics).

use rhai::{Array, Dynamic, Map};
use serde_json::Value as Json;

use crate::scripting::manifest::{FieldType, InputField, Manifest};

/// Errors produced while validating + marshaling a transport input.
#[derive(Debug)]
pub enum InputError {
    /// The top-level JSON value wasn't an object.
    NotAnObject,
    /// A required field was missing and had no default.
    MissingRequired(String),
    /// A field's value didn't match the declared type.
    TypeMismatch { field: String, expected: String, got: String },
    /// An enum field's value wasn't one of the declared variants.
    InvalidEnumVariant { field: String, value: String, variants: Vec<String> },
    /// An array field violated min_items/max_items.
    ArrayBounds { field: String, len: usize, min: Option<usize>, max: Option<usize> },
    /// The caller sent a top-level key the manifest doesn't declare.
    UnknownField(String),
    /// A manifest default was malformed (programmer error: should have been
    /// caught at manifest-parse time, but kept as a runtime check for safety).
    BadDefault(String),
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputError::NotAnObject => write!(f, "input must be a JSON object"),
            InputError::MissingRequired(n) => write!(f, "missing required field `{n}`"),
            InputError::TypeMismatch { field, expected, got } => {
                write!(f, "field `{field}`: expected {expected}, got {got}")
            }
            InputError::InvalidEnumVariant { field, value, variants } => write!(
                f,
                "field `{field}`: {value:?} is not one of {variants:?}"
            ),
            InputError::ArrayBounds { field, len, min, max } => write!(
                f,
                "field `{field}`: array length {len} out of bounds (min={min:?}, max={max:?})"
            ),
            InputError::UnknownField(n) => write!(f, "unknown input field `{n}`"),
            InputError::BadDefault(m) => write!(f, "manifest default: {m}"),
        }
    }
}

impl std::error::Error for InputError {}

/// Validate `input` against `manifest` and convert to a `rhai::Map`.
pub fn marshal_input(manifest: &Manifest, input: &Json) -> Result<Map, InputError> {
    let obj = input.as_object().ok_or(InputError::NotAnObject)?;

    // Reject unknown top-level fields.
    for k in obj.keys() {
        if !manifest.inputs.iter().any(|f| &f.name == k) {
            return Err(InputError::UnknownField(k.clone()));
        }
    }

    let mut out = Map::new();
    for field in &manifest.inputs {
        let value = obj.get(&field.name);
        match value {
            Some(v) => {
                let dyn_v = check_and_convert(&field.field_type, v, &field.name)?;
                if let FieldType::Array(_) = &field.field_type {
                    let len = v.as_array().map(|a| a.len()).unwrap_or(0);
                    check_array_bounds(field, len)?;
                }
                out.insert(field.name.clone().into(), dyn_v);
            }
            None => {
                if let Some(default) = &field.default {
                    let dyn_v = toml_to_dyn(&field.field_type, default, &field.name)?;
                    if let FieldType::Array(_) = &field.field_type {
                        let len = default.as_array().map(|a| a.len()).unwrap_or(0);
                        check_array_bounds(field, len)?;
                    }
                    out.insert(field.name.clone().into(), dyn_v);
                } else if field.required {
                    return Err(InputError::MissingRequired(field.name.clone()));
                }
                // optional + no default → omit; script sees `input.<name> == ()`.
            }
        }
    }
    Ok(out)
}

fn check_array_bounds(field: &InputField, len: usize) -> Result<(), InputError> {
    if let Some(min) = field.min_items {
        if len < min {
            return Err(InputError::ArrayBounds {
                field: field.name.clone(),
                len,
                min: field.min_items,
                max: field.max_items,
            });
        }
    }
    if let Some(max) = field.max_items {
        if len > max {
            return Err(InputError::ArrayBounds {
                field: field.name.clone(),
                len,
                min: field.min_items,
                max: field.max_items,
            });
        }
    }
    Ok(())
}

fn check_and_convert(ty: &FieldType, v: &Json, field: &str) -> Result<Dynamic, InputError> {
    match (ty, v) {
        (FieldType::String, Json::String(s)) => Ok(Dynamic::from(s.clone())),
        (FieldType::Bool, Json::Bool(b)) => Ok(Dynamic::from_bool(*b)),
        (FieldType::Int, Json::Number(n)) => match n.as_i64() {
            Some(i) => Ok(Dynamic::from_int(i)),
            None => Err(InputError::TypeMismatch {
                field: field.into(),
                expected: "int (i64)".into(),
                got: format!("number {n}"),
            }),
        },
        (FieldType::Array(inner), Json::Array(items)) => {
            let mut out = Array::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                let elem_field = format!("{field}[{i}]");
                out.push(check_and_convert(inner, item, &elem_field)?);
            }
            Ok(Dynamic::from(out))
        }
        (FieldType::Enum(variants), Json::String(s)) => {
            if variants.iter().any(|v| v == s) {
                Ok(Dynamic::from(s.clone()))
            } else {
                Err(InputError::InvalidEnumVariant {
                    field: field.into(),
                    value: s.clone(),
                    variants: variants.clone(),
                })
            }
        }
        (FieldType::MapStringString, Json::Object(obj)) => {
            let mut m = Map::new();
            for (k, v) in obj {
                match v {
                    Json::String(s) => {
                        m.insert(k.clone().into(), Dynamic::from(s.clone()));
                    }
                    other => {
                        return Err(InputError::TypeMismatch {
                            field: format!("{field}.{k}"),
                            expected: "string".into(),
                            got: json_kind(other).into(),
                        });
                    }
                }
            }
            Ok(Dynamic::from(m))
        }
        (ty, v) => Err(InputError::TypeMismatch {
            field: field.into(),
            expected: format!("{ty:?}"),
            got: json_kind(v).into(),
        }),
    }
}

fn json_kind(v: &Json) -> &'static str {
    match v {
        Json::Null => "null",
        Json::Bool(_) => "bool",
        Json::Number(_) => "number",
        Json::String(_) => "string",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    }
}

fn toml_to_dyn(ty: &FieldType, v: &toml::Value, field: &str) -> Result<Dynamic, InputError> {
    match (ty, v) {
        (FieldType::String, toml::Value::String(s)) => Ok(Dynamic::from(s.clone())),
        (FieldType::Bool, toml::Value::Boolean(b)) => Ok(Dynamic::from_bool(*b)),
        (FieldType::Int, toml::Value::Integer(i)) => Ok(Dynamic::from_int(*i)),
        (FieldType::Array(inner), toml::Value::Array(items)) => {
            let mut out = Array::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                out.push(toml_to_dyn(inner, item, &format!("{field}[{i}]"))?);
            }
            Ok(Dynamic::from(out))
        }
        (FieldType::Enum(variants), toml::Value::String(s)) if variants.iter().any(|v| v == s) => {
            Ok(Dynamic::from(s.clone()))
        }
        (FieldType::MapStringString, toml::Value::Table(t)) => {
            let mut m = Map::new();
            for (k, v) in t {
                let toml::Value::String(s) = v else {
                    return Err(InputError::BadDefault(format!(
                        "{field}.{k}: not a string"
                    )));
                };
                m.insert(k.clone().into(), Dynamic::from(s.clone()));
            }
            Ok(Dynamic::from(m))
        }
        _ => Err(InputError::BadDefault(format!(
            "field {field}: default does not match type {ty:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripting::manifest::parse;
    use serde_json::json;

    fn mam() -> Manifest {
        parse(
            r#"
name = "myanonamouse"
canonical_category = "myanonamouse.net"
description = "x"

[[input]]
name = "url"
type = "string"
required = true

[[input]]
name = "categories"
type = "array<string>"
required = true
min_items = 1
max_items = 8

[[input]]
name = "author"
type = "string"
required = false

[[input]]
name = "labels"
type = "array<string>"
required = false
default = []

[[input]]
name = "format"
type = "enum<MP3, FLAC, AAC>"
required = false

[[input]]
name = "headers"
type = "map<string, string>"
required = false
"#,
        )
        .unwrap()
    }

    #[test]
    fn happy_path_marshals_all_types() {
        let m = mam();
        let j = json!({
            "url": "https://t/123",
            "categories": ["Computer/Internet", "Education"],
            "author": "Hamza",
            "labels": ["LLM"],
            "format": "FLAC",
            "headers": {"X-Foo": "bar"}
        });
        let map = marshal_input(&m, &j).unwrap();
        assert_eq!(map.get("url").unwrap().clone().into_string().unwrap(), "https://t/123");
        assert_eq!(
            map.get("author").unwrap().clone().into_string().unwrap(),
            "Hamza"
        );
        let cats: rhai::Array = map.get("categories").unwrap().clone().try_cast().unwrap();
        assert_eq!(cats.len(), 2);
    }

    #[test]
    fn missing_required_rejected() {
        let m = mam();
        let j = json!({"categories": ["x"]});
        let err = marshal_input(&m, &j).unwrap_err();
        assert!(matches!(err, InputError::MissingRequired(ref n) if n == "url"));
    }

    #[test]
    fn missing_optional_omitted() {
        let m = mam();
        let j = json!({"url": "u", "categories": ["c"]});
        let map = marshal_input(&m, &j).unwrap();
        assert!(!map.contains_key("author"));
        assert!(!map.contains_key("format"));
        // Default fires for `labels`.
        assert!(map.contains_key("labels"));
    }

    #[test]
    fn default_applied_for_array() {
        let m = mam();
        let j = json!({"url": "u", "categories": ["c"]});
        let map = marshal_input(&m, &j).unwrap();
        let labels: rhai::Array = map.get("labels").unwrap().clone().try_cast().unwrap();
        assert!(labels.is_empty());
    }

    #[test]
    fn type_mismatch_rejected() {
        let m = mam();
        let j = json!({"url": 42, "categories": ["c"]});
        let err = marshal_input(&m, &j).unwrap_err();
        assert!(matches!(err, InputError::TypeMismatch { .. }));
    }

    #[test]
    fn array_bounds_min_violated() {
        let m = mam();
        let j = json!({"url": "u", "categories": []});
        let err = marshal_input(&m, &j).unwrap_err();
        assert!(matches!(err, InputError::ArrayBounds { len: 0, .. }));
    }

    #[test]
    fn array_bounds_max_violated() {
        let m = mam();
        let j = json!({
            "url": "u",
            "categories": ["a","b","c","d","e","f","g","h","i"],
        });
        let err = marshal_input(&m, &j).unwrap_err();
        assert!(matches!(err, InputError::ArrayBounds { len: 9, .. }));
    }

    #[test]
    fn enum_variant_validated() {
        let m = mam();
        let j = json!({"url": "u", "categories": ["c"], "format": "WAV"});
        let err = marshal_input(&m, &j).unwrap_err();
        assert!(matches!(err, InputError::InvalidEnumVariant { .. }));
    }

    #[test]
    fn unknown_field_rejected() {
        let m = mam();
        let j = json!({"url": "u", "categories": ["c"], "bogus": 1});
        let err = marshal_input(&m, &j).unwrap_err();
        assert!(matches!(err, InputError::UnknownField(ref n) if n == "bogus"));
    }

    #[test]
    fn map_string_string_non_string_value_rejected() {
        let m = mam();
        let j = json!({"url": "u", "categories": ["c"], "headers": {"k": 1}});
        let err = marshal_input(&m, &j).unwrap_err();
        assert!(matches!(err, InputError::TypeMismatch { .. }));
    }

    #[test]
    fn nested_array_marshals() {
        let m = parse(
            r#"
name = "t"
canonical_category = "x"
description = "y"

[[input]]
name = "matrix"
type = "array<array<int>>"
required = true
"#,
        )
        .unwrap();
        let j = json!({"matrix": [[1, 2], [3, 4]]});
        let map = marshal_input(&m, &j).unwrap();
        let outer: rhai::Array = map.get("matrix").unwrap().clone().try_cast().unwrap();
        assert_eq!(outer.len(), 2);
        let inner: rhai::Array = outer[0].clone().try_cast().unwrap();
        assert_eq!(inner.len(), 2);
    }

    #[test]
    fn integration_with_classify() {
        use crate::scripting::host::{compile, run_classify};
        use crate::scripting::sandbox::{build_engine, SandboxLimits};

        let m = mam();
        let j = json!({
            "url": "https://t/123",
            "categories": ["Computer/Internet"],
            "author": "Hamza"
        });
        let input = marshal_input(&m, &j).unwrap();

        let eng = build_engine(&SandboxLimits::default());
        let src = r#"
            fn classify(input) {
                let tags = [];
                for cat in input.categories {
                    let path = if input.author != () {
                        cat + "/" + input.author
                    } else { cat };
                    tags.push("link:" + path);
                }
                #{ link_tags: tags, info_tags: [], warnings: [] }
            }
        "#;
        let ast = compile(&eng, src).unwrap();
        let out = run_classify(&eng, &ast, input, &m.canonical_category).unwrap();
        assert_eq!(
            out.link_tags,
            vec!["link:Computer/Internet/Hamza".to_string()]
        );
    }
}
