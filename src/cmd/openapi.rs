//! OpenAPI 3.1 document builder for the REST surface (DESIGN.md §7 `tql api`).
//!
//! Generated at startup from the loaded tracker manifests. Per-tracker paths
//! (`/trackers/<name>/add`, `/trackers/<name>/schema`) get concrete request
//! schemas referenced from `components.schemas.<Name>Input`, so clients see
//! the actual shape they must POST rather than a generic `object`.
//!
//! Manual translation (no `utoipa` dep) — we already build JSON Schemas by
//! hand in `scripting::schema`, and the OpenAPI surface is small enough that
//! a dep buys little.

use serde_json::{json, Map, Value};

use crate::scripting::registry::Registry;
use crate::scripting::schema::to_json_schema;

/// Build a fresh OpenAPI 3.1 document describing the live REST surface.
pub fn build_openapi(registry: &Registry, auth_required: bool) -> Value {
    let mut paths = Map::new();
    paths.insert("/health".into(), health_path());
    paths.insert("/trackers".into(), trackers_index_path());

    let mut schemas = Map::new();
    schemas.insert("ErrorResponse".into(), error_schema());
    schemas.insert("SourceRequest".into(), source_request_schema());
    schemas.insert("AddAck".into(), add_ack_schema());
    schemas.insert("TrackerSummary".into(), tracker_summary_schema());

    for (name, tracker) in registry.iter() {
        let input_schema = to_json_schema(&tracker.manifest);
        let input_schema = strip_root_schema_keys(input_schema);
        let input_ref = format!("{}Input", pascal_case(name));
        schemas.insert(input_ref.clone(), input_schema);

        let add_req_ref = format!("{}AddRequest", pascal_case(name));
        schemas.insert(add_req_ref.clone(), add_request_schema_for(&input_ref));

        paths.insert(
            format!("/trackers/{name}/schema"),
            tracker_schema_path(name, &tracker.manifest.description),
        );
        paths.insert(
            format!("/trackers/{name}/add"),
            tracker_add_path(
                name,
                &tracker.manifest.canonical_category,
                &tracker.manifest.description,
                &add_req_ref,
            ),
        );
    }

    let mut components = Map::new();
    components.insert("schemas".into(), Value::Object(schemas));
    if auth_required {
        let mut sec_schemes = Map::new();
        sec_schemes.insert(
            "bearerAuth".into(),
            json!({ "type": "http", "scheme": "bearer" }),
        );
        sec_schemes.insert(
            "apiKeyAuth".into(),
            json!({ "type": "apiKey", "in": "header", "name": "X-Api-Key" }),
        );
        components.insert("securitySchemes".into(), Value::Object(sec_schemes));
    }

    let mut root = Map::new();
    root.insert("openapi".into(), json!("3.1.0"));
    root.insert(
        "info".into(),
        json!({
            "title": "tql REST API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Tracker-Qualified Layout — per-tracker add operations.",
        }),
    );
    root.insert("paths".into(), Value::Object(paths));
    root.insert("components".into(), Value::Object(components));
    if auth_required {
        // Apply both schemes as alternatives at the document level; /health
        // overrides this with `security: []` to stay public.
        root.insert(
            "security".into(),
            json!([{ "bearerAuth": [] }, { "apiKeyAuth": [] }]),
        );
    }
    Value::Object(root)
}

fn health_path() -> Value {
    json!({
        "get": {
            "summary": "Liveness probe",
            "security": [],
            "responses": {
                "200": {
                    "description": "Server is up",
                    "content": {
                        "application/json": {
                            "schema": {
                                "type": "object",
                                "properties": { "ok": { "type": "boolean" } },
                                "required": ["ok"]
                            }
                        }
                    }
                }
            }
        }
    })
}

fn trackers_index_path() -> Value {
    json!({
        "get": {
            "summary": "List registered trackers",
            "responses": {
                "200": {
                    "description": "Tracker list",
                    "content": {
                        "application/json": {
                            "schema": {
                                "type": "object",
                                "properties": {
                                    "trackers": {
                                        "type": "array",
                                        "items": { "$ref": "#/components/schemas/TrackerSummary" }
                                    }
                                },
                                "required": ["trackers"]
                            }
                        }
                    }
                },
                "401": error_response_ref(),
                "403": error_response_ref()
            }
        }
    })
}

fn tracker_schema_path(name: &str, description: &str) -> Value {
    json!({
        "get": {
            "summary": format!("JSON Schema for tracker {name}"),
            "description": description,
            "responses": {
                "200": {
                    "description": "JSON Schema (draft 2020-12) for the tracker's input",
                    "content": {
                        "application/json": { "schema": { "type": "object" } }
                    }
                },
                "401": error_response_ref(),
                "403": error_response_ref(),
                "404": error_response_ref()
            }
        }
    })
}

fn tracker_add_path(name: &str, category: &str, description: &str, add_req_ref: &str) -> Value {
    json!({
        "post": {
            "summary": format!("Add a torrent via tracker {name}"),
            "description": format!("{description}\n\nCanonical category: {category}"),
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": {
                        "schema": { "$ref": format!("#/components/schemas/{add_req_ref}") }
                    }
                }
            },
            "responses": {
                "200": {
                    "description": "Torrent submitted to qBittorrent",
                    "content": {
                        "application/json": {
                            "schema": { "$ref": "#/components/schemas/AddAck" }
                        }
                    }
                },
                "400": error_response_ref(),
                "401": error_response_ref(),
                "403": error_response_ref(),
                "404": error_response_ref(),
                "502": error_response_ref(),
                "503": error_response_ref()
            }
        }
    })
}

fn add_request_schema_for(input_ref: &str) -> Value {
    json!({
        "type": "object",
        "required": ["input", "source"],
        "additionalProperties": false,
        "properties": {
            "input": { "$ref": format!("#/components/schemas/{input_ref}") },
            "source": { "$ref": "#/components/schemas/SourceRequest" }
        }
    })
}

fn source_request_schema() -> Value {
    json!({
        "type": "object",
        "description": "Torrent source. Tagged by `kind`.",
        "oneOf": [
            {
                "type": "object",
                "required": ["kind", "path"],
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "const": "file" },
                    "path": { "type": "string" }
                }
            },
            {
                "type": "object",
                "required": ["kind", "url"],
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "const": "url" },
                    "url": { "type": "string", "format": "uri" }
                }
            },
            {
                "type": "object",
                "required": ["kind", "uri"],
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "const": "magnet" },
                    "uri": { "type": "string" }
                }
            }
        ]
    })
}

fn add_ack_schema() -> Value {
    json!({
        "type": "object",
        "required": ["ok", "tracker", "category", "link_tags", "info_tags", "warnings", "source"],
        "properties": {
            "ok": { "type": "boolean" },
            "tracker": { "type": "string" },
            "category": { "type": "string" },
            "info_hash": { "type": ["string", "null"] },
            "link_tags": { "type": "array", "items": { "type": "string" } },
            "info_tags": { "type": "array", "items": { "type": "string" } },
            "warnings": { "type": "array", "items": { "type": "string" } },
            "source": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["file", "url", "magnet"] },
                    "value": { "type": "string" }
                },
                "required": ["kind", "value"]
            }
        }
    })
}

fn tracker_summary_schema() -> Value {
    json!({
        "type": "object",
        "required": ["name", "canonical_category", "description"],
        "properties": {
            "name": { "type": "string" },
            "canonical_category": { "type": "string" },
            "description": { "type": "string" }
        }
    })
}

fn error_schema() -> Value {
    json!({
        "type": "object",
        "required": ["error"],
        "properties": { "error": { "type": "string" } }
    })
}

fn error_response_ref() -> Value {
    json!({
        "description": "Error",
        "content": {
            "application/json": {
                "schema": { "$ref": "#/components/schemas/ErrorResponse" }
            }
        }
    })
}

/// Remove the `$schema`/`title`/`description` keys that `to_json_schema`
/// adds at the root — they belong on a standalone document, not on a
/// component schema embedded inside another doc.
fn strip_root_schema_keys(mut v: Value) -> Value {
    if let Some(obj) = v.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
        // Keep description — it documents the tracker input.
    }
    v
}

/// Convert a tracker name (snake/kebab-case) to PascalCase for use as a
/// component schema name. Non-alphanumeric chars split words; digits are
/// preserved.
fn pascal_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut next_upper = true;
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            if next_upper {
                out.extend(ch.to_uppercase());
                next_upper = false;
            } else {
                out.push(ch);
            }
        } else {
            next_upper = true;
        }
    }
    if out.is_empty() {
        out.push_str("Tracker");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripting::registry::{load_dir, LoadReport};
    use crate::scripting::sandbox::{build_engine, SandboxLimits};
    use std::path::PathBuf;

    fn make_registry_with(name: &str) -> Registry {
        let dir: PathBuf = std::env::temp_dir().join(format!(
            "tql-openapi-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let t_dir = dir.join(name);
        std::fs::create_dir_all(&t_dir).unwrap();
        let manifest = format!(
            r#"
name = "{name}"
canonical_category = "{name}.example"
description = "demo tracker"

[[input]]
name = "url"
type = "string"
required = true
description = "url"
"#
        );
        std::fs::write(t_dir.join("manifest.toml"), manifest).unwrap();
        std::fs::write(
            t_dir.join("classify.rhai"),
            r#"
fn classify(input) {
    #{ link_tags: ["link:books"], info_tags: ["info:x"], warnings: [] }
}
"#,
        )
        .unwrap();
        let engine = build_engine(&SandboxLimits::default());
        let LoadReport { registry, .. } = load_dir(&dir, &engine).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        registry
    }

    #[test]
    fn pascal_case_handles_separators() {
        assert_eq!(pascal_case("my_tracker"), "MyTracker");
        assert_eq!(pascal_case("my-tracker"), "MyTracker");
        assert_eq!(pascal_case("foo.bar"), "FooBar");
        assert_eq!(pascal_case("abc123"), "Abc123");
        assert_eq!(pascal_case(""), "Tracker");
    }

    #[test]
    fn empty_registry_still_produces_valid_doc() {
        let doc = build_openapi(&Registry::default(), false);
        assert_eq!(doc["openapi"], "3.1.0");
        assert!(doc["paths"]["/health"].is_object());
        assert!(doc["paths"]["/trackers"].is_object());
        assert!(doc["components"]["schemas"]["ErrorResponse"].is_object());
        // No security at doc level when auth is open.
        assert!(doc.get("security").is_none());
    }

    #[test]
    fn registry_adds_per_tracker_paths_and_schemas() {
        let reg = make_registry_with("demo");
        let doc = build_openapi(&reg, false);
        assert!(doc["paths"]["/trackers/demo/schema"].is_object());
        let add = &doc["paths"]["/trackers/demo/add"]["post"];
        assert!(add.is_object());
        let body_ref = &add["requestBody"]["content"]["application/json"]["schema"]["$ref"];
        assert_eq!(body_ref, "#/components/schemas/DemoAddRequest");

        let schemas = &doc["components"]["schemas"];
        assert!(schemas["DemoInput"].is_object());
        assert!(schemas["DemoAddRequest"].is_object());
        // Stripped from embedded component.
        assert!(schemas["DemoInput"].get("$schema").is_none());
        assert!(schemas["DemoInput"].get("title").is_none());
        // Input schema retained shape.
        assert_eq!(schemas["DemoInput"]["properties"]["url"]["type"], "string");
    }

    #[test]
    fn auth_required_emits_security_schemes() {
        let doc = build_openapi(&Registry::default(), true);
        let schemes = &doc["components"]["securitySchemes"];
        assert_eq!(schemes["bearerAuth"]["scheme"], "bearer");
        assert_eq!(schemes["apiKeyAuth"]["name"], "X-Api-Key");
        let sec = doc["security"].as_array().unwrap();
        assert_eq!(sec.len(), 2);
        // /health is exempt from auth.
        let health_sec = &doc["paths"]["/health"]["get"]["security"];
        assert_eq!(health_sec, &json!([]));
    }

    #[test]
    fn source_request_has_three_variants() {
        let doc = build_openapi(&Registry::default(), false);
        let variants = doc["components"]["schemas"]["SourceRequest"]["oneOf"]
            .as_array()
            .unwrap();
        assert_eq!(variants.len(), 3);
        let kinds: Vec<&str> = variants
            .iter()
            .map(|v| v["properties"]["kind"]["const"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"file"));
        assert!(kinds.contains(&"url"));
        assert!(kinds.contains(&"magnet"));
    }
}
