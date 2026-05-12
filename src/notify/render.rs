//! Notification rendering pipeline (DESIGN.md §15.1).
//!
//! `EventFields -> FieldManipulation (Rhai) -> Template (Handlebars)
//! -> NotificationText`. Stage 1 is a sandboxed Rhai `shape(fields,
//! escape, target)` function; stage 2 is a Handlebars template
//! rendered against the post-manipulation map. Embedded defaults
//! reproduce the pre-Leg-58 hardcoded Telegram output byte-for-byte.

use std::path::{Path, PathBuf};

use handlebars::Handlebars;
use rhai::{Array, Dynamic, Engine, FnPtr, Map, Scope, AST};

use super::Event;
use crate::scripting::sandbox::{build_engine, SandboxLimits};

const DEFAULT_SCRIPT: &str = include_str!("defaults/notify.rhai");
const DEFAULT_TEMPLATE: &str = include_str!("defaults/notify.hbs");

/// Resolved render-source pair for one drainer invocation. Either side
/// may be `None`, in which case the embedded default is used.
#[derive(Debug, Clone, Default)]
pub struct RenderConfig {
    pub script_path: Option<PathBuf>,
    pub template_path: Option<PathBuf>,
}

impl RenderConfig {
    pub fn embedded() -> Self {
        Self::default()
    }
}

/// Backend target — selects the escape function and a `target` tag the
/// script can branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTarget {
    Html,
    MarkdownV2,
    Plain,
}

impl RenderTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            RenderTarget::Html => "HTML",
            RenderTarget::MarkdownV2 => "MarkdownV2",
            RenderTarget::Plain => "Plain",
        }
    }

    pub fn from_parse_mode(mode: &str) -> Self {
        match mode {
            "HTML" => RenderTarget::Html,
            "MarkdownV2" => RenderTarget::MarkdownV2,
            _ => RenderTarget::Plain,
        }
    }

    fn escape(self, s: &str) -> String {
        match self {
            RenderTarget::Html => html_escape(s),
            RenderTarget::MarkdownV2 => markdownv2_escape(s),
            RenderTarget::Plain => s.to_string(),
        }
    }
}

#[derive(Debug)]
pub enum RenderError {
    Compile(String),
    Runtime(String),
    Template(String),
    BadReturnShape(String),
    /// Override path was configured but the file is missing/unreadable.
    OverrideMissing { path: PathBuf, source: String },
    /// Override file loaded but failed to compile/parse.
    OverrideInvalid { path: PathBuf, source: String },
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::Compile(e) => write!(f, "notify script compile error: {e}"),
            RenderError::Runtime(e) => write!(f, "notify script runtime error: {e}"),
            RenderError::Template(e) => write!(f, "notify template error: {e}"),
            RenderError::BadReturnShape(e) => write!(f, "notify shape() returned: {e}"),
            RenderError::OverrideMissing { path, source } => {
                write!(f, "notify override missing at {}: {source}", path.display())
            }
            RenderError::OverrideInvalid { path, source } => {
                write!(f, "notify override invalid at {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for RenderError {}

/// Render one event using the embedded defaults (no overrides).
pub fn render_event(event: &Event, target: RenderTarget) -> Result<String, RenderError> {
    render_batch(std::slice::from_ref(event), target)
}

/// Render a batch of events using the embedded defaults.
pub fn render_batch(events: &[Event], target: RenderTarget) -> Result<String, RenderError> {
    render_batch_with(events, target, &RenderConfig::embedded())
}

/// Render a batch of events, applying any configured global overrides.
/// Override sides resolve independently — operators may swap only the
/// script or only the template.
pub fn render_batch_with(
    events: &[Event],
    target: RenderTarget,
    cfg: &RenderConfig,
) -> Result<String, RenderError> {
    if events.is_empty() {
        return Ok(String::new());
    }
    let (engine, ast, hb) = build_pipeline(target, cfg)?;
    let mut out = String::new();
    for (i, ev) in events.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&render_with(&engine, &ast, &hb, ev, target)?);
    }
    Ok(out)
}

fn read_override(path: &Path) -> Result<String, RenderError> {
    std::fs::read_to_string(path).map_err(|e| RenderError::OverrideMissing {
        path: path.to_path_buf(),
        source: e.to_string(),
    })
}

fn build_pipeline(
    target: RenderTarget,
    cfg: &RenderConfig,
) -> Result<(Engine, AST, Handlebars<'static>), RenderError> {
    let mut engine = build_engine(&SandboxLimits::default());
    engine.register_fn("__tql_escape", move |s: &str| -> String {
        target.escape(s)
    });

    let script_src = match &cfg.script_path {
        Some(p) => read_override(p)?,
        None => DEFAULT_SCRIPT.to_string(),
    };
    let ast = engine.compile(&script_src).map_err(|e| match &cfg.script_path {
        Some(p) => RenderError::OverrideInvalid {
            path: p.clone(),
            source: e.to_string(),
        },
        None => RenderError::Compile(e.to_string()),
    })?;

    let template_src = match &cfg.template_path {
        Some(p) => read_override(p)?,
        None => DEFAULT_TEMPLATE.to_string(),
    };
    let mut hb = Handlebars::new();
    hb.set_strict_mode(false);
    hb.register_escape_fn(handlebars::no_escape);
    hb.register_template_string("notify", &template_src)
        .map_err(|e| match &cfg.template_path {
            Some(p) => RenderError::OverrideInvalid {
                path: p.clone(),
                source: e.to_string(),
            },
            None => RenderError::Template(e.to_string()),
        })?;
    Ok((engine, ast, hb))
}

fn render_with(
    engine: &Engine,
    ast: &AST,
    hb: &Handlebars<'_>,
    event: &Event,
    target: RenderTarget,
) -> Result<String, RenderError> {
    let fields = event_to_map(event);

    // The script invokes `escape(s)` via this FnPtr; `__tql_escape` is
    // registered on the engine and closes over the render target.
    let escape_ptr = FnPtr::new("__tql_escape")
        .map_err(|e| RenderError::Runtime(format!("escape ptr: {e}")))?;

    let mut scope = Scope::new();
    let result: Dynamic = engine
        .call_fn(
            &mut scope,
            ast,
            "shape",
            (fields, escape_ptr, target.as_str().to_string()),
        )
        .map_err(|e| RenderError::Runtime(e.to_string()))?;

    let map = result
        .try_cast::<Map>()
        .ok_or_else(|| RenderError::BadReturnShape("shape() must return an object map".into()))?;

    let json = map_to_json(map);
    let rendered = hb
        .render("notify", &json)
        .map_err(|e| RenderError::Template(e.to_string()))?;
    Ok(strip_trailing_newline(rendered))
}

fn event_to_map(event: &Event) -> Map {
    let mut m = Map::new();
    m.insert("schema_version".into(), Dynamic::from(event.schema_version as i64));
    m.insert("ts".into(), Dynamic::from(event.ts.clone()));
    m.insert(
        "info_hash_v1".into(),
        Dynamic::from(event.info_hash_v1.clone()),
    );
    m.insert("name".into(), Dynamic::from(event.name.clone()));
    m.insert("category".into(), Dynamic::from(event.category.clone()));
    m.insert(
        "link_sites_added".into(),
        Dynamic::from(strings_to_array(&event.link_sites_added)),
    );
    m.insert(
        "link_sites_removed".into(),
        Dynamic::from(strings_to_array(&event.link_sites_removed)),
    );
    m.insert(
        "warnings".into(),
        Dynamic::from(strings_to_array(&event.warnings)),
    );
    m
}

fn strings_to_array(v: &[String]) -> Array {
    v.iter().map(|s| Dynamic::from(s.clone())).collect()
}

fn map_to_json(map: Map) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for (k, v) in map {
        obj.insert(k.to_string(), dynamic_to_json(v));
    }
    serde_json::Value::Object(obj)
}

fn dynamic_to_json(d: Dynamic) -> serde_json::Value {
    use serde_json::Value;
    if d.is_unit() {
        return Value::Null;
    }
    if let Ok(b) = d.as_bool() {
        return Value::Bool(b);
    }
    if let Ok(i) = d.as_int() {
        return Value::Number(i.into());
    }
    if let Ok(f) = d.as_float() {
        return serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null);
    }
    if d.is_string() {
        return Value::String(d.into_string().unwrap_or_default());
    }
    if d.is_array() {
        let arr = d.into_array().unwrap_or_default();
        return Value::Array(arr.into_iter().map(dynamic_to_json).collect());
    }
    if d.is_map() {
        let m = d.try_cast::<Map>().unwrap_or_default();
        return map_to_json(m);
    }
    Value::Null
}

fn strip_trailing_newline(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
    }
    s
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            _ => out.push(c),
        }
    }
    out
}

/// Telegram MarkdownV2 reserved characters per Bot API docs. The pre-Leg-58
/// hardcoded path didn't escape these at all (it fell through to plain text);
/// the default template likewise produces no MarkdownV2 markup, so the
/// escape is only meaningful when an operator opts into a custom template
/// that injects MarkdownV2 syntax.
fn markdownv2_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '_' | '*' | '[' | ']' | '(' | ')' | '~' | '`' | '>' | '#' | '+' | '-' | '=' | '|' | '{' | '}' | '.' | '!' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::EVENT_SCHEMA_VERSION;

    fn ev_full() -> Event {
        Event {
            schema_version: EVENT_SCHEMA_VERSION,
            ts: "2026-05-12T00:00:00Z".into(),
            info_hash_v1: "abc".into(),
            name: "Some Book".into(),
            category: "tracker.tld".into(),
            link_sites_added: vec!["Computer/Hamza".into(), "Math/Knuth".into()],
            link_sites_removed: vec!["Old/Site".into()],
            warnings: vec!["soft cap".into()],
        }
    }

    fn ev_minimal() -> Event {
        Event {
            schema_version: EVENT_SCHEMA_VERSION,
            ts: "2026-05-12T00:00:00Z".into(),
            info_hash_v1: "abc".into(),
            name: "Plain Name".into(),
            category: "x.y".into(),
            link_sites_added: vec![],
            link_sites_removed: vec![],
            warnings: vec![],
        }
    }

    /// Legacy hardcoded formatter — kept in tests as the regression oracle.
    fn legacy_format(events: &[Event], parse_mode: &str) -> String {
        let mut out = String::new();
        for (i, ev) in events.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            match parse_mode {
                "HTML" => {
                    out.push_str(&format!(
                        "<b>{}</b>\n<i>{}</i>",
                        html_escape(&ev.name),
                        html_escape(&ev.category),
                    ));
                    for site in &ev.link_sites_added {
                        out.push_str("\n+ ");
                        out.push_str(&html_escape(site));
                    }
                    for site in &ev.link_sites_removed {
                        out.push_str("\n- ");
                        out.push_str(&html_escape(site));
                    }
                    for w in &ev.warnings {
                        out.push_str("\n⚠ ");
                        out.push_str(&html_escape(w));
                    }
                }
                _ => {
                    out.push_str(&format!("{} [{}]", ev.name, ev.category));
                    for site in &ev.link_sites_added {
                        out.push_str("\n+ ");
                        out.push_str(site);
                    }
                    for site in &ev.link_sites_removed {
                        out.push_str("\n- ");
                        out.push_str(site);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn html_single_event_matches_legacy() {
        let events = vec![ev_full()];
        let new = render_batch(&events, RenderTarget::Html).unwrap();
        let old = legacy_format(&events, "HTML");
        assert_eq!(new, old);
    }

    #[test]
    fn html_html_escapes_unsafe_chars() {
        let mut e = ev_full();
        e.name = "A & <B>".into();
        let new = render_batch(&[e.clone()], RenderTarget::Html).unwrap();
        let old = legacy_format(&[e], "HTML");
        assert_eq!(new, old);
        assert!(new.contains("A &amp; &lt;B&gt;"));
    }

    #[test]
    fn html_batch_two_events_matches_legacy() {
        let events = vec![ev_full(), ev_minimal()];
        let new = render_batch(&events, RenderTarget::Html).unwrap();
        let old = legacy_format(&events, "HTML");
        assert_eq!(new, old);
    }

    #[test]
    fn plain_target_matches_legacy_fallback() {
        let events = vec![ev_full()];
        let new = render_batch(&events, RenderTarget::Plain).unwrap();
        let old = legacy_format(&events, "anything-not-HTML");
        assert_eq!(new, old);
    }

    #[test]
    fn markdownv2_target_matches_legacy_fallback() {
        // Pre-Leg-58, anything not "HTML" used the plain-text fallback.
        // The default script keeps that for MarkdownV2 (skips warnings,
        // emits "name [cat]") so existing operators see no change.
        let events = vec![ev_minimal()];
        let new = render_batch(&events, RenderTarget::MarkdownV2).unwrap();
        let old = legacy_format(&events, "MarkdownV2");
        assert_eq!(new, old);
    }

    #[test]
    fn empty_batch_returns_empty_string() {
        let s = render_batch(&[], RenderTarget::Html).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn render_event_matches_one_element_batch() {
        let e = ev_full();
        let one = render_event(&e, RenderTarget::Html).unwrap();
        let batch = render_batch(&[e], RenderTarget::Html).unwrap();
        assert_eq!(one, batch);
    }

    fn tmp_path(tag: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        d.push(format!("tql-render-{tag}-{}-{n}", std::process::id()));
        d
    }

    #[test]
    fn template_override_replaces_default_output() {
        let dir = tmp_path("tpl");
        std::fs::create_dir_all(&dir).unwrap();
        let tpl = dir.join("notify.hbs");
        std::fs::write(&tpl, "TPL:{{name}}|{{category}}").unwrap();
        let cfg = RenderConfig {
            script_path: None,
            template_path: Some(tpl),
        };
        let out = render_batch_with(&[ev_minimal()], RenderTarget::Html, &cfg).unwrap();
        assert_eq!(out, "TPL:Plain Name|x.y");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_script_override_surfaces_override_missing() {
        let cfg = RenderConfig {
            script_path: Some(PathBuf::from("/no/such/notify.rhai")),
            template_path: None,
        };
        let err = render_batch_with(&[ev_minimal()], RenderTarget::Html, &cfg).unwrap_err();
        assert!(matches!(err, RenderError::OverrideMissing { .. }), "{err:?}");
    }

    #[test]
    fn invalid_script_override_surfaces_override_invalid() {
        let dir = tmp_path("bad");
        std::fs::create_dir_all(&dir).unwrap();
        let s = dir.join("notify.rhai");
        std::fs::write(&s, "fn shape(fields, escape, target) { @@@ }").unwrap();
        let cfg = RenderConfig {
            script_path: Some(s),
            template_path: None,
        };
        let err = render_batch_with(&[ev_minimal()], RenderTarget::Html, &cfg).unwrap_err();
        assert!(matches!(err, RenderError::OverrideInvalid { .. }), "{err:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn embedded_render_config_matches_default_helpers() {
        let ev = ev_full();
        let a = render_batch(std::slice::from_ref(&ev), RenderTarget::Html).unwrap();
        let b = render_batch_with(
            std::slice::from_ref(&ev),
            RenderTarget::Html,
            &RenderConfig::embedded(),
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn target_from_parse_mode_maps_known_values() {
        assert_eq!(RenderTarget::from_parse_mode("HTML"), RenderTarget::Html);
        assert_eq!(
            RenderTarget::from_parse_mode("MarkdownV2"),
            RenderTarget::MarkdownV2
        );
        assert_eq!(RenderTarget::from_parse_mode("Markdown"), RenderTarget::Plain);
        assert_eq!(RenderTarget::from_parse_mode(""), RenderTarget::Plain);
    }
}
