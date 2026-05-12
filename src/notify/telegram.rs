//! Telegram Bot API backend for `tql notify-flush`.
//!
//! One HTTP `POST` per batch to `<base>/bot<token>/sendMessage` with
//! `chat_id`, `text`, `parse_mode`. The base URL is injectable for tests;
//! production uses `https://api.telegram.org`.
//!
//! Errors are returned, never panicked: the caller (`notify-flush`)
//! requeues the failed batch and surfaces the message on stderr.

#![allow(dead_code)]

use std::time::Duration;

use super::render::{render_batch_with, RenderConfig, RenderTarget};
use super::Event;

pub const DEFAULT_BASE_URL: &str = "https://api.telegram.org";
/// Per DESIGN.md §15.
pub const MAX_BATCH: usize = 10;

#[derive(Debug)]
pub enum TelegramError {
    Http(reqwest::Error),
    Api { status: u16, body: String },
}

impl std::fmt::Display for TelegramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TelegramError::Http(e) => write!(f, "telegram http error: {e}"),
            TelegramError::Api { status, body } => {
                write!(f, "telegram api error (HTTP {status}): {body}")
            }
        }
    }
}

impl std::error::Error for TelegramError {}

/// Format a batch of events as a single Telegram message body, delegating
/// to the §15.1 render pipeline. On render failure (a broken default
/// would be a build-time bug, but tracker/global overrides land in later
/// legs and may fail at runtime), fall back to a minimal one-line summary
/// per event so the drainer can still ship a notification.
pub fn format_message(events: &[Event], parse_mode: &str) -> String {
    format_message_with(events, parse_mode, &RenderConfig::embedded())
}

/// Variant that applies a configured `RenderConfig` (global overrides
/// from `[notify]`). On override failure the drainer logs a WARN and
/// retries with the embedded defaults before falling back to the
/// minimal one-line summary (DESIGN §15.2).
pub fn format_message_with(
    events: &[Event],
    parse_mode: &str,
    cfg: &RenderConfig,
) -> String {
    let target = RenderTarget::from_parse_mode(parse_mode);
    match render_batch_with(events, target, cfg) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "notify render failed with overrides, retrying embedded defaults");
            match render_batch_with(events, target, &RenderConfig::embedded()) {
                Ok(s) => s,
                Err(e2) => {
                    tracing::warn!(error = %e2, "embedded notify render failed, using minimal fallback");
                    minimal_fallback(events)
                }
            }
        }
    }
}

/// Render a batch with three-level fallback: per-tracker override pair
/// (already merged onto global by the caller) → global-only → embedded →
/// minimal one-line. Used when per-tracker overrides are in play; a busted
/// tracker override falls back to global with a WARN rather than going all
/// the way to embedded.
pub fn format_message_chain(
    events: &[Event],
    parse_mode: &str,
    resolved: &RenderConfig,
    global: &RenderConfig,
) -> String {
    let target = RenderTarget::from_parse_mode(parse_mode);
    if let Ok(s) = render_batch_with(events, target, resolved) {
        return s;
    }
    tracing::warn!("notify render failed with tracker override, retrying global");
    if let Ok(s) = render_batch_with(events, target, global) {
        return s;
    }
    tracing::warn!("notify render failed with global override, retrying embedded");
    if let Ok(s) = render_batch_with(events, target, &RenderConfig::embedded()) {
        return s;
    }
    tracing::warn!("embedded notify render failed, using minimal fallback");
    minimal_fallback(events)
}

/// Group events by their effective `RenderConfig`, render each run with
/// the three-level fallback chain, and join with newlines. Preserves the
/// input order within each run; runs themselves appear in the order their
/// first event was seen.
pub fn format_message_grouped<F>(
    events: &[Event],
    parse_mode: &str,
    global: &RenderConfig,
    mut resolver: F,
) -> String
where
    F: FnMut(&Event) -> RenderConfig,
{
    if events.is_empty() {
        return String::new();
    }
    // Group consecutive events sharing the same (script_path, template_path)
    // pair. Adjacent same-tracker bursts (the common case) stay in one
    // pipeline build; interleaved categories produce more runs but each is
    // still rendered correctly.
    let mut out = String::new();
    let mut run: Vec<Event> = Vec::new();
    let mut run_cfg: Option<RenderConfig> = None;
    for ev in events {
        let cfg = resolver(ev);
        let same = run_cfg
            .as_ref()
            .map(|c| c.script_path == cfg.script_path && c.template_path == cfg.template_path)
            .unwrap_or(false);
        if !same && !run.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format_message_chain(
                &run,
                parse_mode,
                run_cfg.as_ref().unwrap(),
                global,
            ));
            run.clear();
        }
        if run.is_empty() {
            run_cfg = Some(cfg);
        }
        run.push(ev.clone());
    }
    if !run.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format_message_chain(
            &run,
            parse_mode,
            run_cfg.as_ref().unwrap(),
            global,
        ));
    }
    out
}

fn minimal_fallback(events: &[Event]) -> String {
    let mut out = String::new();
    for (i, ev) in events.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("{} [{}]", ev.name, ev.category));
    }
    out
}

/// POST one batch to `sendMessage`. Returns `Ok(())` on a 2xx response with
/// `ok: true`, otherwise an error.
pub async fn send_batch(
    base_url: &str,
    bot_token: &str,
    chat_id: &str,
    parse_mode: &str,
    events: &[Event],
    render_cfg: &RenderConfig,
) -> Result<(), TelegramError> {
    let url = format!(
        "{}/bot{}/sendMessage",
        base_url.trim_end_matches('/'),
        bot_token
    );
    let body = serde_json::json!({
        "chat_id": chat_id,
        "text": format_message_with(events, parse_mode, render_cfg),
        "parse_mode": parse_mode,
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(TelegramError::Http)?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(TelegramError::Http)?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(TelegramError::Http)?;
    if !(200..300).contains(&status) {
        return Err(TelegramError::Api { status, body: text });
    }
    // The Bot API always returns 200 OK with `{ok: bool, ...}`; if `ok`
    // is false, surface the description.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
        if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
            return Err(TelegramError::Api { status, body: text });
        }
    }
    Ok(())
}

/// Variant of `send_batch` that resolves a per-event `RenderConfig` via
/// `resolver` and falls back through the tracker → global → embedded
/// chain (`format_message_grouped`). The HTTP behavior is otherwise
/// identical to `send_batch`.
pub async fn send_batch_resolved<F>(
    base_url: &str,
    bot_token: &str,
    chat_id: &str,
    parse_mode: &str,
    events: &[Event],
    global: &RenderConfig,
    resolver: F,
) -> Result<(), TelegramError>
where
    F: FnMut(&Event) -> RenderConfig,
{
    let url = format!(
        "{}/bot{}/sendMessage",
        base_url.trim_end_matches('/'),
        bot_token
    );
    let body = serde_json::json!({
        "chat_id": chat_id,
        "text": format_message_grouped(events, parse_mode, global, resolver),
        "parse_mode": parse_mode,
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(TelegramError::Http)?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(TelegramError::Http)?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(TelegramError::Http)?;
    if !(200..300).contains(&status) {
        return Err(TelegramError::Api { status, body: text });
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
        if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
            return Err(TelegramError::Api { status, body: text });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::{Event, EVENT_SCHEMA_VERSION};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;

    fn ev(name: &str) -> Event {
        Event {
            schema_version: EVENT_SCHEMA_VERSION,
            ts: "2026-05-11T12:00:00Z".into(),
            info_hash_v1: "abc".into(),
            name: name.into(),
            category: "tracker.tld".into(),
            link_sites_added: vec!["Computer/Hamza".into()],
            link_sites_removed: vec![],
            warnings: vec![],
        }
    }

    #[test]
    fn html_escapes_unsafe_chars() {
        let e = Event {
            name: "A & <B>".into(),
            ..ev("ignored")
        };
        let msg = format_message(&[e], "HTML");
        assert!(msg.contains("A &amp; &lt;B&gt;"));
    }

    #[test]
    fn batches_join_with_newline() {
        let msg = format_message(&[ev("a"), ev("b")], "HTML");
        let parts: Vec<&str> = msg.split('\n').collect();
        // Two events × at least 3 lines each (title, category, +site).
        assert!(parts.len() >= 6, "msg: {msg}");
    }

    fn spawn_mock<F>(handler: F) -> (String, Arc<AtomicBool>, thread::JoinHandle<()>)
    where
        F: Fn(&str) -> String + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                if stop_t.load(Ordering::SeqCst) {
                    break;
                }
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let mut buf = [0u8; 8192];
                let mut acc = Vec::new();
                loop {
                    let n = match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    acc.extend_from_slice(&buf[..n]);
                    if let Some(idx) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&acc[..idx]).unwrap_or("");
                        let cl = headers
                            .lines()
                            .find_map(|l| {
                                let l = l.trim().to_ascii_lowercase();
                                l.strip_prefix("content-length:")
                                    .and_then(|r| r.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if acc.len() >= idx + 4 + cl {
                            break;
                        }
                    }
                }
                let req = String::from_utf8_lossy(&acc).to_string();
                let resp = handler(&req);
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        (url, stop, handle)
    }

    fn ok_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn tmp_path(tag: &str) -> std::path::PathBuf {
        let mut d = std::env::temp_dir();
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        d.push(format!("tql-tg-{tag}-{}-{n}", std::process::id()));
        d
    }

    #[test]
    fn grouped_uses_per_event_template() {
        let dir = tmp_path("grouped-tpl");
        std::fs::create_dir_all(&dir).unwrap();
        let a_tpl = dir.join("a.hbs");
        let b_tpl = dir.join("b.hbs");
        std::fs::write(&a_tpl, "A:{{name}}").unwrap();
        std::fs::write(&b_tpl, "B:{{name}}").unwrap();

        let global = RenderConfig::embedded();
        let events = vec![
            Event { name: "one".into(), category: "a.tld".into(), ..ev("ignored") },
            Event { name: "two".into(), category: "b.tld".into(), ..ev("ignored") },
            Event { name: "three".into(), category: "a.tld".into(), ..ev("ignored") },
        ];
        let a_tpl_c = a_tpl.clone();
        let b_tpl_c = b_tpl.clone();
        let out = format_message_grouped(&events, "HTML", &global, |e: &Event| {
            let tpl = match e.category.as_str() {
                "a.tld" => Some(a_tpl_c.clone()),
                "b.tld" => Some(b_tpl_c.clone()),
                _ => None,
            };
            RenderConfig {
                script_path: None,
                template_path: tpl,
            }
        });
        assert_eq!(out, "A:one\nB:two\nA:three");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn grouped_falls_back_to_global_on_broken_tracker_override() {
        // Tracker template path points at a missing file → render with
        // tracker cfg fails → chain falls back to global (here also a
        // working file template) rather than embedded or minimal.
        let dir = tmp_path("grouped-fallback");
        std::fs::create_dir_all(&dir).unwrap();
        let global_tpl = dir.join("global.hbs");
        std::fs::write(&global_tpl, "G:{{name}}").unwrap();

        let global = RenderConfig {
            script_path: None,
            template_path: Some(global_tpl.clone()),
        };
        let events = vec![Event {
            name: "x".into(),
            category: "a.tld".into(),
            ..ev("ignored")
        }];
        let out = format_message_grouped(&events, "HTML", &global, |_| RenderConfig {
            script_path: None,
            template_path: Some(dir.join("does-not-exist.hbs")),
        });
        assert_eq!(out, "G:x");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolved_picks_tracker_over_global_per_side() {
        let global = RenderConfig {
            script_path: Some("/global.rhai".into()),
            template_path: Some("/global.hbs".into()),
        };
        // Tracker provides only a template — script falls through to global.
        let r = RenderConfig::resolved(&global, None, Some("/tracker.hbs".into()));
        assert_eq!(r.script_path.as_deref(), Some(std::path::Path::new("/global.rhai")));
        assert_eq!(
            r.template_path.as_deref(),
            Some(std::path::Path::new("/tracker.hbs"))
        );
        // Tracker provides only a script.
        let r = RenderConfig::resolved(&global, Some("/tracker.rhai".into()), None);
        assert_eq!(
            r.script_path.as_deref(),
            Some(std::path::Path::new("/tracker.rhai"))
        );
        assert_eq!(
            r.template_path.as_deref(),
            Some(std::path::Path::new("/global.hbs"))
        );
    }

    #[tokio::test]
    async fn send_batch_posts_to_send_message() {
        let captured: Arc<std::sync::Mutex<String>> =
            Arc::new(std::sync::Mutex::new(String::new()));
        let cap = captured.clone();
        let (base, _stop, _h) = spawn_mock(move |req| {
            *cap.lock().unwrap() = req.to_string();
            ok_response(r#"{"ok":true,"result":{}}"#)
        });
        send_batch(&base, "TOKEN", "-100", "HTML", &[ev("a")], &RenderConfig::embedded())
            .await
            .expect("ok");
        let req = captured.lock().unwrap().clone();
        assert!(req.contains("POST /botTOKEN/sendMessage"), "req: {req}");
        assert!(req.contains(r#""chat_id":"-100""#));
        assert!(req.contains(r#""parse_mode":"HTML""#));
    }

    #[tokio::test]
    async fn send_batch_surfaces_api_error() {
        let (base, _stop, _h) =
            spawn_mock(|_| ok_response(r#"{"ok":false,"description":"bad chat"}"#));
        let err = send_batch(&base, "T", "x", "HTML", &[ev("a")], &RenderConfig::embedded())
            .await
            .expect_err("should fail");
        match err {
            TelegramError::Api { body, .. } => assert!(body.contains("bad chat")),
            other => panic!("wrong variant: {other}"),
        }
    }

    #[tokio::test]
    async fn send_batch_surfaces_http_error() {
        let (base, _stop, _h) = spawn_mock(|_| {
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 3\r\nConnection: close\r\n\r\nerr".to_string()
        });
        let err = send_batch(&base, "T", "x", "HTML", &[ev("a")], &RenderConfig::embedded())
            .await
            .expect_err("should fail");
        match err {
            TelegramError::Api { status, .. } => assert_eq!(status, 500),
            other => panic!("wrong variant: {other}"),
        }
    }
}
