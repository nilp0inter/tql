//! Manifest → JSON Schema translation (DESIGN.md §7 `GET /trackers/<name>/schema`).
//!
//! Produces a JSON Schema (draft 2020-12) describing the tracker's input
//! object. Field types map straightforwardly to JSON types; manifest
//! constraints (`min_items`, `max_items`, enum variants, defaults) are
//! preserved.

use serde_json::{json, Map, Value};

use super::manifest::{FieldType, InputField, Manifest};

const SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Build a JSON Schema for the tracker's input object.
pub fn to_json_schema(m: &Manifest) -> Value {
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    for field in &m.inputs {
        properties.insert(field.name.clone(), field_schema(field));
        if field.required {
            required.push(Value::String(field.name.clone()));
        }
    }

    let mut root = Map::new();
    root.insert("$schema".into(), Value::String(SCHEMA_DIALECT.into()));
    root.insert("title".into(), Value::String(m.name.clone()));
    root.insert("description".into(), Value::String(m.description.clone()));
    root.insert("type".into(), Value::String("object".into()));
    root.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        root.insert("required".into(), Value::Array(required));
    }
    root.insert("additionalProperties".into(), Value::Bool(false));
    Value::Object(root)
}

fn field_schema(f: &InputField) -> Value {
    let mut obj = match &f.field_type {
        FieldType::String => json_map([("type", json!("string"))]),
        FieldType::Int => json_map([("type", json!("integer"))]),
        FieldType::Bool => json_map([("type", json!("boolean"))]),
        FieldType::Enum(variants) => json_map([
            ("type", json!("string")),
            ("enum", Value::Array(variants.iter().cloned().map(Value::String).collect())),
        ]),
        FieldType::Array(inner) => {
            let mut a = json_map([
                ("type", json!("array")),
                ("items", type_schema(inner)),
            ]);
            if let Some(lo) = f.min_items {
                a.insert("minItems".into(), json!(lo));
            }
            if let Some(hi) = f.max_items {
                a.insert("maxItems".into(), json!(hi));
            }
            a
        }
        FieldType::MapStringString => json_map([
            ("type", json!("object")),
            ("additionalProperties", json!({"type": "string"})),
        ]),
    };

    if !f.description.is_empty() {
        obj.insert("description".into(), Value::String(f.description.clone()));
    }
    if let Some(d) = &f.default {
        obj.insert("default".into(), toml_to_json(d));
    }
    Value::Object(obj)
}

/// Inner-type schema for array items — no per-field constraints carry through.
fn type_schema(t: &FieldType) -> Value {
    match t {
        FieldType::String => json!({"type": "string"}),
        FieldType::Int => json!({"type": "integer"}),
        FieldType::Bool => json!({"type": "boolean"}),
        FieldType::Enum(variants) => json!({
            "type": "string",
            "enum": variants,
        }),
        FieldType::Array(inner) => json!({
            "type": "array",
            "items": type_schema(inner),
        }),
        FieldType::MapStringString => json!({
            "type": "object",
            "additionalProperties": {"type": "string"},
        }),
    }
}

fn toml_to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => json!(*i),
        toml::Value::Float(f) => json!(*f),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(a) => Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            let mut m = Map::new();
            for (k, v) in t {
                m.insert(k.clone(), toml_to_json(v));
            }
            Value::Object(m)
        }
    }
}

fn json_map<const N: usize>(entries: [(&str, Value); N]) -> Map<String, Value> {
    let mut m = Map::new();
    for (k, v) in entries {
        m.insert(k.to_string(), v);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripting::manifest::parse;

    #[test]
    fn primitive_types_map_correctly() {
        let m = parse(
            r#"
name = "t"
canonical_category = "x"
description = "demo"

[[input]]
name = "title"
type = "string"
required = true
description = "the title"

[[input]]
name = "count"
type = "int"

[[input]]
name = "flag"
type = "bool"
default = false
"#,
        )
        .unwrap();
        let s = to_json_schema(&m);
        assert_eq!(s["type"], "object");
        assert_eq!(s["title"], "t");
        assert_eq!(s["description"], "demo");
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["$schema"], SCHEMA_DIALECT);
        assert_eq!(s["properties"]["title"]["type"], "string");
        assert_eq!(s["properties"]["title"]["description"], "the title");
        assert_eq!(s["properties"]["count"]["type"], "integer");
        assert_eq!(s["properties"]["flag"]["type"], "boolean");
        assert_eq!(s["properties"]["flag"]["default"], false);
        let required = s["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0], "title");
    }

    #[test]
    fn array_carries_min_max_and_inner_type() {
        let m = parse(
            r#"
name = "t"
canonical_category = "x"
description = "demo"

[[input]]
name = "tags"
type = "array<string>"
min_items = 1
max_items = 10
"#,
        )
        .unwrap();
        let s = to_json_schema(&m);
        let tags = &s["properties"]["tags"];
        assert_eq!(tags["type"], "array");
        assert_eq!(tags["items"]["type"], "string");
        assert_eq!(tags["minItems"], 1);
        assert_eq!(tags["maxItems"], 10);
    }

    #[test]
    fn enum_emits_enum_array() {
        let m = parse(
            r#"
name = "t"
canonical_category = "x"
description = "demo"

[[input]]
name = "color"
type = "enum<red, green, blue>"
default = "green"
"#,
        )
        .unwrap();
        let s = to_json_schema(&m);
        let c = &s["properties"]["color"];
        assert_eq!(c["type"], "string");
        let variants = c["enum"].as_array().unwrap();
        assert_eq!(variants.len(), 3);
        assert_eq!(variants[0], "red");
        assert_eq!(c["default"], "green");
    }

    #[test]
    fn map_string_string_uses_additional_properties() {
        let m = parse(
            r#"
name = "t"
canonical_category = "x"
description = "demo"

[[input]]
name = "labels"
type = "map<string, string>"
"#,
        )
        .unwrap();
        let s = to_json_schema(&m);
        let labels = &s["properties"]["labels"];
        assert_eq!(labels["type"], "object");
        assert_eq!(labels["additionalProperties"]["type"], "string");
    }

    #[test]
    fn nested_array_recurses() {
        let m = parse(
            r#"
name = "t"
canonical_category = "x"
description = "demo"

[[input]]
name = "matrix"
type = "array<array<int>>"
"#,
        )
        .unwrap();
        let s = to_json_schema(&m);
        let mx = &s["properties"]["matrix"];
        assert_eq!(mx["type"], "array");
        assert_eq!(mx["items"]["type"], "array");
        assert_eq!(mx["items"]["items"]["type"], "integer");
    }

    #[test]
    fn no_required_field_omits_required_key() {
        let m = parse(
            r#"
name = "t"
canonical_category = "x"
description = "demo"

[[input]]
name = "a"
type = "string"
"#,
        )
        .unwrap();
        let s = to_json_schema(&m);
        assert!(s.get("required").is_none());
    }
}
