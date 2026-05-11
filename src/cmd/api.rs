//! `tql api` — REST server (DESIGN.md §7, §13).
//!
//! Endpoints:
//!   * `GET  /health`
//!   * `GET  /trackers`
//!   * `GET  /trackers/<name>/schema` (Leg 13b)
//!   * `POST /trackers/<name>/add`
//!   * `GET  /openapi.json` (Leg 13c)
//!
//! Auth: when `[api].api_key_env` is set in config, every endpoint except
//! `/health` requires either an `Authorization: Bearer <key>` or an
//! `X-Api-Key: <key>` header matching the env var's value. When unset, the
//! server runs open (suitable for localhost-only deployments).

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json2;

use crate::cmd::cli::{
    build_ack, build_add_params, build_torrent_source, SourceKind,
};
use crate::cmd::openapi::build_openapi;
use crate::config::{self, Config};
use crate::qbit;
use crate::qbit::types::TorrentSource;
use crate::scripting::host::run_classify;
use crate::scripting::input::marshal_input;
use crate::scripting::registry::{load_dir, Registry};
use crate::scripting::sandbox::{build_engine, SandboxLimits};
use crate::scripting::schema::to_json_schema;

#[derive(Parser, Debug)]
pub struct Args {
    /// Override the configured listen address.
    #[arg(long, value_name = "ADDR")]
    pub addr: Option<String>,
    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<(), u8> {
    let (_path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("api: {e}");
            return Err(1);
        }
    };

    let engine = build_engine(&SandboxLimits::default());
    let report = match load_dir(&cfg.paths.trackers_root, &engine) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("api: {e}");
            return Err(1);
        }
    };
    for f in &report.failures {
        eprintln!("api: load failure: {f}");
    }

    let api_key = match cfg.api.api_key_env.as_deref() {
        Some(name) => match std::env::var(name) {
            Ok(v) if !v.is_empty() => Some(v),
            Ok(_) | Err(_) => {
                eprintln!("api: env var {name:?} (api_key_env) is not set");
                return Err(1);
            }
        },
        None => None,
    };

    let addr = args.addr.unwrap_or_else(|| cfg.api.addr.clone());
    let state = AppState::new(report.registry, engine, cfg, api_key);
    let app = router(state);

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("api: tokio runtime: {e}");
            return Err(1);
        }
    };

    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("api: bind {addr}: {e}");
                return Err::<(), u8>(1);
            }
        };
        eprintln!("api: listening on http://{addr}");
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("api: serve error: {e}");
            return Err(1);
        }
        Ok(())
    })
}

/// Shared, cheaply-cloneable handler state.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) registry: Arc<Registry>,
    pub(crate) engine: Arc<rhai::Engine>,
    pub(crate) cfg: Arc<Config>,
    pub(crate) api_key: Option<String>,
}

impl AppState {
    pub(crate) fn new(
        registry: Registry,
        engine: rhai::Engine,
        cfg: Config,
        api_key: Option<String>,
    ) -> Self {
        Self {
            registry: Arc::new(registry),
            engine: Arc::new(engine),
            cfg: Arc::new(cfg),
            api_key,
        }
    }
}

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/trackers", get(list_trackers))
        .route("/trackers/:name/schema", get(tracker_schema))
        .route("/trackers/:name/add", post(add_to_tracker))
        .route("/openapi.json", get(openapi_doc))
        .with_state(state)
}

// ---- handlers ------------------------------------------------------------

async fn health() -> Json<Json2> {
    Json(serde_json::json!({ "ok": true }))
}

async fn list_trackers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check_auth(&state, &headers) {
        return r;
    }
    let items: Vec<Json2> = state
        .registry
        .iter()
        .map(|(name, t)| {
            serde_json::json!({
                "name": name,
                "canonical_category": t.manifest.canonical_category,
                "description": t.manifest.description.lines().next().unwrap_or(""),
            })
        })
        .collect();
    (StatusCode::OK, Json(serde_json::json!({ "trackers": items }))).into_response()
}

async fn tracker_schema(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = check_auth(&state, &headers) {
        return r;
    }
    let Some(tracker) = state.registry.get(&name) else {
        return err(StatusCode::NOT_FOUND, format!("tracker {name:?} not registered"));
    };
    (StatusCode::OK, Json(to_json_schema(&tracker.manifest))).into_response()
}

async fn openapi_doc(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = check_auth(&state, &headers) {
        return r;
    }
    let doc = build_openapi(&state.registry, state.api_key.is_some());
    (StatusCode::OK, Json(doc)).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct AddRequest {
    pub input: Json2,
    pub source: SourceRequest,
}

/// Wire shape for the `source` field, matching DESIGN.md §8.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum SourceRequest {
    File { path: String },
    Url { url: String },
    Magnet { uri: String },
}

impl SourceRequest {
    pub(crate) fn value(&self) -> &str {
        match self {
            SourceRequest::File { path } => path,
            SourceRequest::Url { url } => url,
            SourceRequest::Magnet { uri } => uri,
        }
    }
    pub(crate) fn kind(&self) -> SourceKind {
        match self {
            SourceRequest::File { .. } => SourceKind::File,
            SourceRequest::Url { .. } => SourceKind::Url,
            SourceRequest::Magnet { .. } => SourceKind::Magnet,
        }
    }
}

async fn add_to_tracker(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<AddRequest>,
) -> Response {
    if let Err(r) = check_auth(&state, &headers) {
        return r;
    }

    let Some(tracker) = state.registry.get(&name) else {
        return err(StatusCode::NOT_FOUND, format!("tracker {name:?} not registered"));
    };

    let map = match marshal_input(&tracker.manifest, &body.input) {
        Ok(m) => m,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("input error: {e}")),
    };

    let output = match run_classify(
        &state.engine,
        &tracker.script,
        map,
        &tracker.manifest.canonical_category,
    ) {
        Ok(o) => o,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("classify error: {e}")),
    };

    let kind = body.source.kind();
    let source_str = body.source.value().to_string();

    let torrent_source = match build_torrent_source(&source_str, kind) {
        Ok(s) => s,
        Err(e) => {
            return err(
                StatusCode::BAD_REQUEST,
                format!("cannot read source {source_str:?}: {e}"),
            );
        }
    };
    let file_bytes: Option<Vec<u8>> = match &torrent_source {
        TorrentSource::File { bytes, .. } => Some(bytes.clone()),
        _ => None,
    };

    let qb = match state.cfg.qbittorrent.as_ref() {
        Some(q) => q,
        None => {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "config has no [qbittorrent] section".into(),
            );
        }
    };
    let password = match std::env::var(&qb.password_env) {
        Ok(v) => v,
        Err(_) => {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("env var {} is not set", qb.password_env),
            );
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
        return err(StatusCode::BAD_GATEWAY, format!("qbittorrent: {e}"));
    }

    let ack = build_ack(
        &name,
        &tracker.manifest.canonical_category,
        &source_str,
        kind,
        file_bytes.as_deref(),
        &output,
    );
    (StatusCode::OK, Json(ack)).into_response()
}

// ---- auth ----------------------------------------------------------------

/// `Ok(())` to proceed, `Err(response)` to short-circuit. When `api_key` is
/// `None` (no key configured) all requests pass.
pub(crate) fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    let Some(expected) = state.api_key.as_deref() else {
        return Ok(());
    };
    let presented = extract_api_key(headers);
    match presented {
        Some(k) if k == expected => Ok(()),
        Some(_) => Err(err(StatusCode::FORBIDDEN, "invalid api key".into())),
        None => Err(err(
            StatusCode::UNAUTHORIZED,
            "api key required (Authorization: Bearer <key> or X-Api-Key)".into(),
        )),
    }
}

fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(rest) = v.strip_prefix("Bearer ") {
            return Some(rest.trim().to_string());
        }
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
}

fn err(status: StatusCode, message: String) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Api, Linking, Mcp, Notify, Paths, Reconcile, Scripting, Media};
    use crate::scripting::registry::{Registry, Tracker};
    use crate::scripting::manifest;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tower::ServiceExt;

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

    fn make_registry_with(name: &str) -> Registry {
        // Reach into Registry via load_dir on a tempdir.
        let dir = std::env::temp_dir().join(format!(
            "tql-api-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
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
        ).unwrap();
        let engine = build_engine(&SandboxLimits::default());
        let report = load_dir(&dir, &engine).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        report.registry
    }

    fn state_with(registry: Registry, api_key: Option<String>) -> AppState {
        let engine = build_engine(&SandboxLimits::default());
        AppState::new(registry, engine, empty_config(), api_key)
    }

    async fn body_json(resp: Response) -> Json2 {
        let (parts, body) = resp.into_parts();
        let bytes = to_bytes(body, 1 << 20).await.unwrap();
        let v: Json2 = serde_json::from_slice(&bytes).unwrap_or(Json2::Null);
        serde_json::json!({ "status": parts.status.as_u16(), "body": v })
    }

    #[tokio::test]
    async fn health_returns_ok_true() {
        let state = state_with(Registry::default(), None);
        let app = router(state);
        let resp = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let out = body_json(resp).await;
        assert_eq!(out["status"], 200);
        assert_eq!(out["body"]["ok"], Json2::Bool(true));
    }

    #[tokio::test]
    async fn list_trackers_returns_registered_names() {
        let reg = make_registry_with("demo");
        let state = state_with(reg, None);
        let app = router(state);
        let resp = app
            .oneshot(Request::builder().uri("/trackers").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let out = body_json(resp).await;
        assert_eq!(out["status"], 200);
        let arr = out["body"]["trackers"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "demo");
        assert_eq!(arr[0]["canonical_category"], "demo.example");
        assert_eq!(arr[0]["description"], "demo tracker");
    }

    #[tokio::test]
    async fn schema_endpoint_returns_json_schema() {
        let reg = make_registry_with("demo");
        let state = state_with(reg, None);
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/trackers/demo/schema")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let out = body_json(resp).await;
        assert_eq!(out["status"], 200);
        let body = &out["body"];
        assert_eq!(body["type"], "object");
        assert_eq!(body["title"], "demo");
        assert_eq!(body["properties"]["url"]["type"], "string");
        assert_eq!(body["required"].as_array().unwrap()[0], "url");
        assert_eq!(body["additionalProperties"], false);
    }

    #[tokio::test]
    async fn schema_unknown_tracker_404() {
        let state = state_with(Registry::default(), None);
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/trackers/nope/schema")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn schema_requires_auth_when_configured() {
        let state = state_with(make_registry_with("demo"), Some("secret".into()));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/trackers/demo/schema")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_missing_key_when_required_yields_401() {
        let state = state_with(Registry::default(), Some("secret".into()));
        let app = router(state);
        let resp = app
            .oneshot(Request::builder().uri("/trackers").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_wrong_key_yields_403() {
        let state = state_with(Registry::default(), Some("secret".into()));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/trackers")
                    .header("x-api-key", "wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn auth_bearer_token_accepted() {
        let state = state_with(make_registry_with("demo"), Some("secret".into()));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/trackers")
                    .header(header::AUTHORIZATION, "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn health_does_not_require_auth_check_logic() {
        // /health is intentionally exempt from check_auth so liveness probes
        // don't need credentials.
        let state = state_with(Registry::default(), Some("secret".into()));
        let app = router(state);
        let resp = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn add_unknown_tracker_404() {
        let state = state_with(Registry::default(), None);
        let app = router(state);
        let body = serde_json::json!({
            "input": {"url": "https://x"},
            "source": {"kind": "magnet", "uri": "magnet:?xt=urn:btih:abc"}
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/trackers/nope/add")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn add_classify_succeeds_then_503_without_qbittorrent() {
        let reg = make_registry_with("demo");
        let state = state_with(reg, None);
        let app = router(state);
        let body = serde_json::json!({
            "input": {"url": "https://x"},
            "source": {"kind": "magnet", "uri": "magnet:?xt=urn:btih:abc"}
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/trackers/demo/add")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        // No [qbittorrent] in cfg → 503 (classify passed, qbittorrent missing).
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn add_bad_input_400() {
        let reg = make_registry_with("demo");
        let state = state_with(reg, None);
        let app = router(state);
        // Missing required `url` field.
        let body = serde_json::json!({
            "input": {},
            "source": {"kind": "magnet", "uri": "magnet:?xt=urn:btih:abc"}
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/trackers/demo/add")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn openapi_endpoint_returns_doc() {
        let reg = make_registry_with("demo");
        let state = state_with(reg, None);
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let out = body_json(resp).await;
        assert_eq!(out["status"], 200);
        assert_eq!(out["body"]["openapi"], "3.1.0");
        assert!(out["body"]["paths"]["/trackers/demo/add"].is_object());
        assert!(out["body"]["paths"]["/trackers/demo/schema"].is_object());
    }

    #[tokio::test]
    async fn openapi_requires_auth_when_configured() {
        let state = state_with(Registry::default(), Some("secret".into()));
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn source_request_serde_roundtrip() {
        let s: SourceRequest =
            serde_json::from_str(r#"{"kind":"file","path":"/tmp/x.torrent"}"#).unwrap();
        assert_eq!(s.kind(), SourceKind::File);
        assert_eq!(s.value(), "/tmp/x.torrent");

        let s: SourceRequest =
            serde_json::from_str(r#"{"kind":"url","url":"https://x/y.torrent"}"#).unwrap();
        assert_eq!(s.kind(), SourceKind::Url);

        let s: SourceRequest =
            serde_json::from_str(r#"{"kind":"magnet","uri":"magnet:?xt=urn:btih:abc"}"#).unwrap();
        assert_eq!(s.kind(), SourceKind::Magnet);
    }

    // Suppress unused warnings on imports kept for clarity.
    #[allow(dead_code)]
    fn _imports() {
        let _ = manifest::FieldType::String;
        let _ = Tracker {
            manifest: manifest::parse(
                r#"
name = "x"
canonical_category = "x"
description = "x"
[[input]]
name = "a"
type = "string"
required = true
description = "a"
"#,
            )
            .unwrap(),
            script: Arc::new(rhai::AST::empty()),
            dir: PathBuf::from("/tmp"),
        };
    }
}
