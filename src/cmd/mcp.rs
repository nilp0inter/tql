//! `tql mcp` — MCP server (DESIGN.md §7, §13).
//!
//! Implements the Model Context Protocol as JSON-RPC 2.0 over either stdio
//! (line-delimited frames; one request per line) or HTTP (`POST /` with a
//! single JSON-RPC frame per request, `204 No Content` for notifications).
//!
//! Tools are registered dynamically from the tracker registry: each tracker
//! `<name>` becomes one tool `tracker.<name>.add` whose `inputSchema` is
//! `{ input: <manifest schema>, source: <SourceRequest> }`.
//!
//! On `tools/call`, the same classify → qBittorrent pipeline as `tql cli` /
//! `tql api` runs; the acknowledgment is returned as a single TextContent.
//! Failures bubble up as `isError: true` tool results (not JSON-RPC errors)
//! so the MCP client sees a structured failure per the spec.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use clap::Parser;
use serde_json::{json, Map as JsonMap, Value as Json};

use crate::cmd::cli::{build_ack, build_add_params, SourceKind};
use crate::config::{self, Config};
use crate::qbit;
use crate::qbit::types::TorrentSource;
use crate::scripting::host::run_classify;
use crate::scripting::input::marshal_input;
use crate::scripting::registry::{load_dir, RegistryHandle};
use crate::scripting::sandbox::{build_engine, SandboxLimits};
use crate::scripting::schema::to_json_schema;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "tql";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
pub struct Args {
    /// Use stdio transport (default).
    #[arg(long, conflicts_with = "http")]
    pub stdio: bool,
    /// Listen on this address with the HTTP transport. (Not implemented yet.)
    #[arg(long, value_name = "ADDR")]
    pub http: Option<String>,
    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<(), u8> {
    let (_path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("mcp: {e}");
            return Err(1);
        }
    };

    let engine = build_engine(&SandboxLimits::default());
    let report = match load_dir(&cfg.paths.trackers_root, &engine) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mcp: {e}");
            return Err(1);
        }
    };
    for f in &report.failures {
        eprintln!("mcp: load failure: {f}");
    }

    let api_key = match cfg.mcp.api_key_env.as_deref() {
        Some(name) => match std::env::var(name) {
            Ok(v) if !v.is_empty() => Some(v),
            Ok(_) | Err(_) => {
                eprintln!("mcp: env var {name:?} (api_key_env) is not set");
                return Err(1);
            }
        },
        None => None,
    };

    let server = Server {
        registry: RegistryHandle::new(report.registry),
        engine: Arc::new(engine),
        cfg: Arc::new(cfg),
        api_key,
    };

    if let Some(addr) = args.http.clone() {
        return run_http(server, addr);
    }

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mcp: tokio runtime: {e}");
            return Err(1);
        }
    };

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    eprintln!("mcp: stdio transport ready (protocol {MCP_PROTOCOL_VERSION})");
    tracing::info!(
        role = "mcp",
        transport = "stdio",
        protocol = MCP_PROTOCOL_VERSION,
        "ready"
    );

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("mcp: read error: {e}");
                return Err(1);
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = rt.block_on(server.handle_line(&line));
        if let Some(resp) = response {
            let payload = match serde_json::to_string(&resp) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("mcp: serialize error: {e}");
                    continue;
                }
            };
            if let Err(e) = writeln!(out, "{payload}").and_then(|_| out.flush()) {
                eprintln!("mcp: write error: {e}");
                return Err(1);
            }
        }
    }

    Ok(())
}

/// Cheaply-cloneable server state; shared across requests on the same stdio
/// connection (and, eventually, across tasks in HTTP mode).
#[derive(Clone)]
pub(crate) struct Server {
    pub(crate) registry: RegistryHandle,
    pub(crate) engine: Arc<rhai::Engine>,
    pub(crate) cfg: Arc<Config>,
    /// When `Some`, HTTP requests must present this key. Stdio transport
    /// is unauthenticated regardless: it's already a trusted local pipe.
    pub(crate) api_key: Option<String>,
}

impl Server {
    /// Parse a single JSON-RPC frame, dispatch it, and return the reply to
    /// serialize back to the client. Returns `None` for notifications (no
    /// `id` present) per JSON-RPC 2.0.
    pub(crate) async fn handle_line(&self, line: &str) -> Option<Json> {
        let msg: Json = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(error_response(
                    Json::Null,
                    -32700,
                    format!("parse error: {e}"),
                ));
            }
        };

        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Json::Null);

        // Notifications (no id) get no reply. JSON-RPC 2.0 §4.1.
        let is_notification = id.is_none();

        let result = match method {
            "initialize" => Ok(self.handle_initialize()),
            "initialized" | "notifications/initialized" => {
                // Spec-mandated notification acknowledging the handshake.
                return None;
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.handle_tools_list()),
            "tools/call" => self.handle_tools_call(params).await,
            other => Err((-32601, format!("method not found: {other}"))),
        };

        if is_notification {
            return None;
        }
        let id = id.unwrap_or(Json::Null);
        Some(match result {
            Ok(v) => success_response(id, v),
            Err((code, msg)) => error_response(id, code, msg),
        })
    }

    fn handle_initialize(&self) -> Json {
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
        })
    }

    pub(crate) fn handle_tools_list(&self) -> Json {
        let registry = self.registry.load();
        let tools: Vec<Json> = registry
            .iter()
            .map(|(name, t)| {
                json!({
                    "name": tool_name_for(name),
                    "description": t.manifest.description,
                    "inputSchema": tool_input_schema(&t.manifest),
                })
            })
            .collect();
        json!({ "tools": tools })
    }

    pub(crate) async fn handle_tools_call(&self, params: Json) -> Result<Json, (i64, String)> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or((-32602, "missing `name`".into()))?;
        let tracker_name = match tool_name_to_tracker(name) {
            Some(n) => n,
            None => return Ok(tool_error(format!("unknown tool: {name}"))),
        };
        let registry = self.registry.load();
        let Some(tracker) = registry.get(tracker_name) else {
            return Ok(tool_error(format!(
                "tracker {tracker_name:?} not registered"
            )));
        };
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

        let input = arguments.get("input").cloned().unwrap_or(json!({}));
        let source = match parse_source(arguments.get("source")) {
            Ok(s) => s,
            Err(e) => return Ok(tool_error(e)),
        };

        let map = match marshal_input(&tracker.manifest, &input) {
            Ok(m) => m,
            Err(e) => return Ok(tool_error(format!("input error: {e}"))),
        };
        let output = match run_classify(
            &self.engine,
            &tracker.script,
            map,
            &tracker.manifest.canonical_category,
        ) {
            Ok(o) => o,
            Err(e) => return Ok(tool_error(format!("classify error: {e}"))),
        };

        let torrent_source = match crate::cmd::cli::resolve_torrent_source(
            &self.cfg,
            tracker_name,
            &source.value,
            source.kind,
        )
        .await
        {
            Ok(s) => s,
            Err(e @ crate::cmd::cli::ResolveError::KindNotAllowed { .. }) => {
                return Ok(tool_error(format!("{e}")));
            }
            Err(e) => {
                return Ok(tool_error(format!(
                    "cannot read source {:?}: {e}",
                    source.value
                )));
            }
        };
        let file_bytes: Option<Vec<u8>> = match &torrent_source {
            TorrentSource::File { bytes, .. } => Some(bytes.clone()),
            _ => None,
        };

        let qb = match self.cfg.qbittorrent.as_ref() {
            Some(q) => q,
            None => {
                return Ok(tool_error("config has no [qbittorrent] section".into()));
            }
        };
        let password = match std::env::var(&qb.password_env) {
            Ok(v) => v,
            Err(_) => {
                return Ok(tool_error(format!(
                    "env var {} is not set",
                    qb.password_env
                )));
            }
        };

        let params = build_add_params(&tracker.manifest.canonical_category, &output);
        let submit: Result<(), qbit::Error> = async {
            let client = qbit::Client::new(&qb.url)?;
            client.login(&qb.username, &password).await?;
            client.add_torrent(torrent_source, &params).await?;
            Ok(())
        }
        .await;
        if let Err(e) = submit {
            return Ok(tool_error(format!("qbittorrent: {e}")));
        }

        let ack = build_ack(
            tracker_name,
            &tracker.manifest.canonical_category,
            &source.value,
            source.kind,
            file_bytes.as_deref(),
            &output,
        );
        Ok(tool_success(&ack))
    }
}

// ---- HTTP transport ------------------------------------------------------

fn run_http(server: Server, addr: String) -> Result<(), u8> {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mcp: tokio runtime: {e}");
            return Err(1);
        }
    };
    let trackers_root = server.cfg.paths.trackers_root.clone();
    let registry_handle = server.registry.clone();
    let engine_handle = server.engine.clone();
    let app = http_router(server);
    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("mcp: bind {addr}: {e}");
                return Err::<(), u8>(1);
            }
        };
        eprintln!("mcp: http transport listening on http://{addr}");
        tracing::info!(role = "mcp", transport = "http", %addr, "listening");

        let pid_path = match crate::pidfile::write("mcp") {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("mcp: pidfile: {e} (continuing without)");
                None
            }
        };

        spawn_reload_listener_mcp(registry_handle, engine_handle, trackers_root);

        let result = tokio::select! {
            r = axum::serve(listener, app) => r.map_err(|e| { eprintln!("mcp: serve error: {e}"); 1u8 }),
            _ = shutdown_signal_mcp() => {
                eprintln!("mcp: shutdown signal received");
                Ok(())
            }
        };

        if pid_path.is_some() {
            let _ = crate::pidfile::remove("mcp");
        }
        result
    })
}

fn spawn_reload_listener_mcp(
    registry: RegistryHandle,
    engine: Arc<rhai::Engine>,
    trackers_root: PathBuf,
) {
    tokio::spawn(async move {
        let mut sig = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mcp: install SIGHUP handler: {e}");
                return;
            }
        };
        while sig.recv().await.is_some() {
            eprintln!(
                "mcp: SIGHUP — reloading registry from {}",
                trackers_root.display()
            );
            match load_dir(&trackers_root, &engine) {
                Ok(report) => {
                    for f in &report.failures {
                        eprintln!("mcp: load failure: {f}");
                    }
                    let n = report.registry.len();
                    registry.swap(report.registry);
                    eprintln!("mcp: registry reloaded ({n} tracker(s))");
                }
                Err(e) => eprintln!("mcp: reload failed: {e}"),
            }
        }
    });
}

async fn shutdown_signal_mcp() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut int = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(_) => return std::future::pending::<()>().await,
    };
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => return std::future::pending::<()>().await,
    };
    tokio::select! {
        _ = int.recv() => {},
        _ = term.recv() => {},
    }
}

pub(crate) fn http_router(server: Server) -> Router {
    Router::new()
        .route("/health", get(http_health))
        .route("/", post(http_jsonrpc))
        .layer(axum::middleware::from_fn(
            crate::cmd::http_trace::trace_request,
        ))
        .with_state(server)
}

async fn http_health() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        json!({ "ok": true }).to_string(),
    )
        .into_response()
}

async fn http_jsonrpc(
    State(server): State<Server>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(r) = check_http_auth(&server, &headers) {
        return r;
    }
    let line = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => {
            return jsonrpc_http_response(error_response(
                Json::Null,
                -32700,
                "parse error: body is not valid UTF-8".into(),
            ));
        }
    };
    match server.handle_line(line).await {
        Some(resp) => jsonrpc_http_response(resp),
        // JSON-RPC 2.0 notification → no body. HTTP: 204.
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

fn jsonrpc_http_response(value: Json) -> Response {
    let body = serde_json::to_string(&value).unwrap_or_else(|_| value.to_string());
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

fn check_http_auth(server: &Server, headers: &HeaderMap) -> Result<(), Response> {
    let Some(expected) = server.api_key.as_deref() else {
        return Ok(());
    };
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string())
        });
    match presented {
        Some(k) if k == expected => Ok(()),
        Some(_) => Err((
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "application/json")],
            json!({ "error": "invalid api key" }).to_string(),
        )
            .into_response()),
        None => Err((
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            json!({ "error": "api key required" }).to_string(),
        )
            .into_response()),
    }
}

/// Map a tracker name → MCP tool name. Format mandated by DESIGN.md §7.
fn tool_name_for(tracker: &str) -> String {
    format!("tracker.{tracker}.add")
}

/// Inverse of [`tool_name_for`]. Returns `None` if the tool name doesn't
/// match the `tracker.<name>.add` shape.
fn tool_name_to_tracker(tool: &str) -> Option<&str> {
    let rest = tool.strip_prefix("tracker.")?;
    rest.strip_suffix(".add")
}

/// Build the `inputSchema` for a tracker's tool: `{ input, source }`.
pub(crate) fn tool_input_schema(manifest: &crate::scripting::manifest::Manifest) -> Json {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["input", "source"],
        "properties": {
            "input": to_json_schema(manifest),
            "source": source_schema(),
        },
    })
}

fn source_schema() -> Json {
    json!({
        "oneOf": [
            { "type": "object", "additionalProperties": false,
              "required": ["kind", "path"],
              "properties": {
                "kind": { "const": "file" },
                "path": { "type": "string" }
              }
            },
            { "type": "object", "additionalProperties": false,
              "required": ["kind", "url"],
              "properties": {
                "kind": { "const": "url" },
                "url": { "type": "string" }
              }
            },
            { "type": "object", "additionalProperties": false,
              "required": ["kind", "uri"],
              "properties": {
                "kind": { "const": "magnet" },
                "uri": { "type": "string" }
              }
            },
        ]
    })
}

struct ParsedSource {
    kind: SourceKind,
    value: String,
}

fn parse_source(value: Option<&Json>) -> Result<ParsedSource, String> {
    let v = value.ok_or_else(|| "missing `source`".to_string())?;
    let kind = v
        .get("kind")
        .and_then(|k| k.as_str())
        .ok_or_else(|| "source.kind missing".to_string())?;
    match kind {
        "file" => {
            let p = v
                .get("path")
                .and_then(|p| p.as_str())
                .ok_or_else(|| "source.path missing for kind=file".to_string())?;
            Ok(ParsedSource {
                kind: SourceKind::File,
                value: p.to_string(),
            })
        }
        "url" => {
            let u = v
                .get("url")
                .and_then(|p| p.as_str())
                .ok_or_else(|| "source.url missing for kind=url".to_string())?;
            Ok(ParsedSource {
                kind: SourceKind::Url,
                value: u.to_string(),
            })
        }
        "magnet" => {
            let u = v
                .get("uri")
                .and_then(|p| p.as_str())
                .ok_or_else(|| "source.uri missing for kind=magnet".to_string())?;
            Ok(ParsedSource {
                kind: SourceKind::Magnet,
                value: u.to_string(),
            })
        }
        other => Err(format!("source.kind {other:?} not in {{file,url,magnet}}")),
    }
}

fn success_response(id: Json, result: Json) -> Json {
    let mut obj = JsonMap::new();
    obj.insert("jsonrpc".into(), Json::String("2.0".into()));
    obj.insert("id".into(), id);
    obj.insert("result".into(), result);
    Json::Object(obj)
}

fn error_response(id: Json, code: i64, message: String) -> Json {
    let mut obj = JsonMap::new();
    obj.insert("jsonrpc".into(), Json::String("2.0".into()));
    obj.insert("id".into(), id);
    obj.insert("error".into(), json!({ "code": code, "message": message }));
    Json::Object(obj)
}

/// Tool-level success: wrap the ack JSON as a single TextContent block.
fn tool_success(ack: &Json) -> Json {
    let text = serde_json::to_string_pretty(ack).unwrap_or_else(|_| ack.to_string());
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": false,
    })
}

/// Tool-level error per MCP spec — a normal result with `isError: true`,
/// not a JSON-RPC error.
fn tool_error(message: String) -> Json {
    json!({
        "content": [ { "type": "text", "text": message } ],
        "isError": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Api, Linking, Mcp, Media, Notify, Paths, Reconcile, Scripting};
    use crate::scripting::registry::Registry;
    use std::collections::BTreeMap;

    fn empty_config() -> Config {
        Config {
            paths: Paths {
                seed_root: PathBuf::from("/tmp"),
                library_root: PathBuf::from("/tmp"),
                trackers_root: PathBuf::from("/tmp"),
            },
            linking: Linking::default(),
            qbittorrent: None,
            notify: Notify::default(),
            media: Media::default(),
            reconcile: Reconcile::default(),
            mcp: Mcp::default(),
            api: Api::default(),
            scripting: Scripting::default(),
            trackers: BTreeMap::new(),
        }
    }

    fn make_registry(name: &str) -> Registry {
        let dir = std::env::temp_dir().join(format!(
            "tql-mcp-test-{}-{}",
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
        let report = load_dir(&dir, &engine).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        report.registry
    }

    fn make_server(registry: Registry) -> Server {
        Server {
            registry: RegistryHandle::new(registry),
            engine: Arc::new(build_engine(&SandboxLimits::default())),
            cfg: Arc::new(empty_config()),
            api_key: None,
        }
    }

    #[test]
    fn tool_name_roundtrip() {
        assert_eq!(tool_name_for("demo"), "tracker.demo.add");
        assert_eq!(tool_name_to_tracker("tracker.demo.add"), Some("demo"));
        assert_eq!(tool_name_to_tracker("tracker.demo"), None);
        assert_eq!(tool_name_to_tracker("foo.bar.add"), None);
    }

    #[tokio::test]
    async fn initialize_returns_protocol_version() {
        let server = make_server(Registry::default());
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let resp = server.handle_line(req).await.unwrap();
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], "tql");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn initialized_notification_yields_no_reply() {
        let server = make_server(Registry::default());
        // No `id` → notification.
        let req = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert!(server.handle_line(req).await.is_none());
    }

    #[tokio::test]
    async fn tools_list_includes_each_registered_tracker() {
        let server = make_server(make_registry("demo"));
        let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let resp = server.handle_line(req).await.unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "tracker.demo.add");
        assert_eq!(tools[0]["description"], "demo tracker");
        let schema = &tools[0]["inputSchema"];
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["input"].is_object());
        assert!(schema["properties"]["source"].is_object());
        assert_eq!(
            schema["properties"]["input"]["properties"]["url"]["type"],
            "string"
        );
    }

    #[tokio::test]
    async fn tools_call_unknown_tracker_returns_iserror() {
        let server = make_server(Registry::default());
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "tracker.nope.add",
                "arguments": {
                    "input": {"url": "https://x"},
                    "source": {"kind": "magnet", "uri": "magnet:?xt=urn:btih:abc"}
                }
            }
        })
        .to_string();
        let resp = server.handle_line(&req).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"nope\""));
    }

    #[tokio::test]
    async fn tools_call_classify_succeeds_then_qbit_missing_is_iserror() {
        let server = make_server(make_registry("demo"));
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "tracker.demo.add",
                "arguments": {
                    "input": {"url": "https://x"},
                    "source": {"kind": "magnet", "uri": "magnet:?xt=urn:btih:abc"}
                }
            }
        })
        .to_string();
        let resp = server.handle_line(&req).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("qbittorrent"), "unexpected: {text}");
    }

    #[tokio::test]
    async fn tools_call_bad_input_is_iserror() {
        let server = make_server(make_registry("demo"));
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "tracker.demo.add",
                "arguments": {
                    "input": {},  // missing required `url`
                    "source": {"kind": "magnet", "uri": "magnet:?xt=urn:btih:abc"}
                }
            }
        })
        .to_string();
        let resp = server.handle_line(&req).await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("input error"), "unexpected: {text}");
    }

    #[tokio::test]
    async fn unknown_method_returns_jsonrpc_error() {
        let server = make_server(Registry::default());
        let req = r#"{"jsonrpc":"2.0","id":6,"method":"resources/list"}"#;
        let resp = server.handle_line(req).await.unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn parse_error_returns_minus_32700() {
        let server = make_server(Registry::default());
        let resp = server.handle_line("not json").await.unwrap();
        assert_eq!(resp["error"]["code"], -32700);
    }

    // ---- HTTP transport tests -------------------------------------------

    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    fn make_server_with_key(key: Option<&str>) -> Server {
        Server {
            registry: RegistryHandle::new(make_registry("demo")),
            engine: Arc::new(build_engine(&SandboxLimits::default())),
            cfg: Arc::new(empty_config()),
            api_key: key.map(|s| s.to_string()),
        }
    }

    #[tokio::test]
    async fn http_health_is_open() {
        let app = http_router(make_server_with_key(Some("secret")));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: Json = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn http_initialize_roundtrip() {
        let app = http_router(make_server_with_key(None));
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Json = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn http_notification_yields_204() {
        let app = http_router(make_server_with_key(None));
        let body = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn http_parse_error_returns_jsonrpc_minus_32700() {
        let app = http_router(make_server_with_key(None));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::from("not json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        // 200 OK with JSON-RPC error envelope per spec.
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Json = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn http_auth_required_when_key_set() {
        let app = http_router(make_server_with_key(Some("s3cret")));
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("authorization", "Bearer nope")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("authorization", "Bearer s3cret")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Json = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["id"], 1);
        assert!(v["result"].is_object());
    }

    #[tokio::test]
    async fn http_x_api_key_header_also_accepted() {
        let app = http_router(make_server_with_key(Some("s3cret")));
        let body = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("x-api-key", "s3cret")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Json = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["id"], 2);
        let tools = v["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "tracker.demo.add");
    }

    #[test]
    fn parse_source_variants() {
        let f = parse_source(Some(&json!({"kind":"file","path":"/a.torrent"}))).unwrap();
        assert_eq!(f.kind, SourceKind::File);
        assert_eq!(f.value, "/a.torrent");
        let u = parse_source(Some(&json!({"kind":"url","url":"https://x"}))).unwrap();
        assert_eq!(u.kind, SourceKind::Url);
        let m = parse_source(Some(
            &json!({"kind":"magnet","uri":"magnet:?xt=urn:btih:abc"}),
        ))
        .unwrap();
        assert_eq!(m.kind, SourceKind::Magnet);
        assert!(parse_source(None).is_err());
        assert!(parse_source(Some(&json!({"kind":"weird"}))).is_err());
    }
}
