//! `tql config validate` — static-only validator for the loaded config.
//!
//! Sibling of `tql doctor`, but strictly offline: no network probes, no
//! sandboxed fixture runs, no qBittorrent login. Verifies structural shape
//! and cross-section invariants that the bare `serde` deserialization
//! cannot catch on its own.
//!
//! Checks performed:
//! - every `*_env` field referenced by an enabled section names an env var
//!   that is actually set (value never printed);
//! - URL fields parse and use `http`/`https`;
//! - `paths.*` are absolute and resolve to existing directories;
//! - `[mcp]` with `transport = "http"` requires `api_key_env` to be set;
//! - every `[trackers.<name>]` credential block has a matching manifest in
//!   `paths.trackers_root` (static manifest load only, no fixture execution).
//!
//! Exits non-zero on any FAIL. WARNs are advisory.
//!
//! With `--json`, emits the same shape as `tql doctor --json`.

use std::path::PathBuf;

use clap::Parser;

use crate::cmd::doctor::{render_json, Check, Status};
use crate::config::{self, Config, McpTransport};
use crate::scripting::registry::load_dir;
use crate::scripting::sandbox::{build_engine, SandboxLimits};

#[derive(Parser, Debug)]
pub struct Args {
    /// Emit the checklist as a single JSON document instead of human-readable lines.
    #[arg(long)]
    pub json: bool,

    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<(), u8> {
    let mut checks: Vec<Check> = Vec::new();

    let (path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => {
            checks.push(Check {
                name: "config".into(),
                status: Status::Ok(format!("parsed {}", v.0.display())),
            });
            v
        }
        Err(e) => {
            checks.push(Check {
                name: "config".into(),
                status: Status::Fail(e),
            });
            return finish(checks, args.json, &mut std::io::stdout());
        }
    };
    let _ = path;

    checks.extend(check_paths(&cfg));
    checks.extend(check_urls(&cfg));
    checks.extend(check_env_vars(&cfg));
    checks.extend(check_mcp_http(&cfg));
    checks.extend(check_trackers_static(&cfg));

    finish(checks, args.json, &mut std::io::stdout())
}

pub(crate) fn finish<W: std::io::Write>(
    checks: Vec<Check>,
    json: bool,
    out: &mut W,
) -> Result<(), u8> {
    let (fails, warns) = tally(&checks);
    if json {
        let doc = render_json(&checks, fails, warns);
        let _ = writeln!(out, "{}", serde_json::to_string_pretty(&doc).unwrap());
    } else {
        for c in &checks {
            let tag = match &c.status {
                Status::Ok(_) => "OK  ",
                Status::Warn(_) => "WARN",
                Status::Fail(_) => "FAIL",
            };
            let body = match &c.status {
                Status::Ok(b) | Status::Warn(b) | Status::Fail(b) => b.as_str(),
            };
            let _ = writeln!(out, "[{tag}] {:28} {body}", c.name);
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "validate: {} check(s) — {} ok, {} warn, {} fail",
            checks.len(),
            checks.len() - warns - fails,
            warns,
            fails
        );
    }
    if fails > 0 {
        return Err(1);
    }
    Ok(())
}

fn tally(checks: &[Check]) -> (usize, usize) {
    let mut fails = 0usize;
    let mut warns = 0usize;
    for c in checks {
        match c.status {
            Status::Ok(_) => {}
            Status::Warn(_) => warns += 1,
            Status::Fail(_) => fails += 1,
        }
    }
    (fails, warns)
}

fn check_paths(cfg: &Config) -> Vec<Check> {
    let entries = [
        ("paths.seed_root", &cfg.paths.seed_root),
        ("paths.library_root", &cfg.paths.library_root),
        ("paths.trackers_root", &cfg.paths.trackers_root),
    ];
    let mut out = Vec::with_capacity(entries.len());
    for (label, p) in entries {
        if !p.is_absolute() {
            out.push(Check {
                name: label.into(),
                status: Status::Fail(format!("{} is not absolute", p.display())),
            });
            continue;
        }
        match std::fs::metadata(p) {
            Ok(m) if m.is_dir() => out.push(Check {
                name: label.into(),
                status: Status::Ok(format!("{} (dir)", p.display())),
            }),
            Ok(_) => out.push(Check {
                name: label.into(),
                status: Status::Fail(format!("{} is not a directory", p.display())),
            }),
            Err(e) => out.push(Check {
                name: label.into(),
                status: Status::Fail(format!("{}: {e}", p.display())),
            }),
        }
    }
    out
}

fn check_urls(cfg: &Config) -> Vec<Check> {
    let mut out = Vec::new();
    if let Some(q) = &cfg.qbittorrent {
        out.push(check_url("qbittorrent.url", &q.url));
    }
    if let Some(p) = &cfg.media.plex {
        out.push(check_url("media.plex.url", &p.url));
    }
    if let Some(j) = &cfg.media.jellyfin {
        out.push(check_url("media.jellyfin.url", &j.url));
    }
    out
}

fn check_url(label: &str, url: &str) -> Check {
    match reqwest::Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => Check {
            name: label.into(),
            status: Status::Ok(format!("{url} ({} )", u.scheme()).trim_end().into()),
        },
        Ok(u) => Check {
            name: label.into(),
            status: Status::Fail(format!("{url}: scheme {} is not http/https", u.scheme())),
        },
        Err(e) => Check {
            name: label.into(),
            status: Status::Fail(format!("{url}: {e}")),
        },
    }
}

fn check_env_vars(cfg: &Config) -> Vec<Check> {
    let mut out = Vec::new();
    if let Some(q) = &cfg.qbittorrent {
        out.push(env_check("qbittorrent.password_env", &q.password_env));
    }
    if let Some(t) = &cfg.notify.telegram {
        out.push(env_check("notify.telegram.bot_token_env", &t.bot_token_env));
    }
    if let Some(p) = &cfg.media.plex {
        out.push(env_check("media.plex.token_env", &p.token_env));
    }
    if let Some(j) = &cfg.media.jellyfin {
        out.push(env_check("media.jellyfin.api_key_env", &j.api_key_env));
    }
    if let Some(name) = &cfg.api.api_key_env {
        out.push(env_check("api.api_key_env", name));
    }
    if let Some(name) = &cfg.mcp.api_key_env {
        out.push(env_check("mcp.api_key_env", name));
    }
    for (tname, creds) in &cfg.trackers {
        if let Some(name) = &creds.cookie_env {
            out.push(env_check(
                &format!("trackers.{tname}.cookie_env"),
                name,
            ));
        }
        if let Some(name) = &creds.auth_header_env {
            out.push(env_check(
                &format!("trackers.{tname}.auth_header_env"),
                name,
            ));
        }
    }
    out
}

fn env_check(label: &str, var: &str) -> Check {
    match std::env::var(var) {
        Ok(_) => Check {
            name: label.into(),
            status: Status::Ok(format!("{var} set")),
        },
        Err(_) => Check {
            name: label.into(),
            status: Status::Fail(format!("env {var} not set")),
        },
    }
}

fn check_mcp_http(cfg: &Config) -> Vec<Check> {
    if !matches!(cfg.mcp.transport, McpTransport::Http) {
        return Vec::new();
    }
    match &cfg.mcp.api_key_env {
        Some(_) => vec![Check {
            name: "mcp.http".into(),
            status: Status::Ok("transport=http with api_key_env set".into()),
        }],
        None => vec![Check {
            name: "mcp.http".into(),
            status: Status::Fail(
                "transport = \"http\" requires mcp.api_key_env to be set".into(),
            ),
        }],
    }
}

fn check_trackers_static(cfg: &Config) -> Vec<Check> {
    if cfg.trackers.is_empty() {
        return Vec::new();
    }
    let limits = SandboxLimits::default();
    let engine = build_engine(&limits);
    let report = match load_dir(&cfg.paths.trackers_root, &engine) {
        Ok(r) => r,
        Err(e) => {
            return vec![Check {
                name: "trackers.root".into(),
                status: Status::Fail(format!(
                    "{}: {e}",
                    cfg.paths.trackers_root.display()
                )),
            }];
        }
    };
    let mut out = Vec::new();
    let known: std::collections::BTreeSet<&str> = report.registry.names().collect();
    for tname in cfg.trackers.keys() {
        if known.contains(tname.as_str()) {
            out.push(Check {
                name: format!("trackers.{tname}"),
                status: Status::Ok("manifest present".into()),
            });
        } else {
            out.push(Check {
                name: format!("trackers.{tname}"),
                status: Status::Fail(format!(
                    "no manifest matches credentials key under {}",
                    cfg.paths.trackers_root.display()
                )),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Api, Jellyfin, Linking, Mcp, Media, Notify, Paths, Plex, QBittorrent, Reconcile,
        Scripting, Telegram, TrackerCreds,
    };
    use std::collections::BTreeMap;

    fn tmp() -> PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!(
            "tql-validate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn base_cfg(root: &std::path::Path) -> Config {
        let seed = root.join("seed");
        let lib = root.join("lib");
        let trackers = root.join("trackers");
        for p in [&seed, &lib, &trackers] {
            std::fs::create_dir_all(p).unwrap();
        }
        Config {
            paths: Paths {
                seed_root: seed,
                library_root: lib,
                trackers_root: trackers,
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

    #[test]
    fn paths_pass_when_absolute_and_present() {
        let root = tmp();
        let cfg = base_cfg(&root);
        let checks = check_paths(&cfg);
        for c in &checks {
            assert!(matches!(c.status, Status::Ok(_)), "{:?}", c);
        }
    }

    #[test]
    fn paths_fail_when_relative() {
        let mut cfg = base_cfg(&tmp());
        cfg.paths.seed_root = PathBuf::from("relative/seed");
        let checks = check_paths(&cfg);
        assert!(checks.iter().any(|c| c.name == "paths.seed_root"
            && matches!(c.status, Status::Fail(ref m) if m.contains("not absolute"))));
    }

    #[test]
    fn urls_must_be_http_or_https() {
        let root = tmp();
        let mut cfg = base_cfg(&root);
        cfg.qbittorrent = Some(QBittorrent {
            url: "ftp://example.com".into(),
            username: "x".into(),
            password_env: "X".into(),
        });
        let checks = check_urls(&cfg);
        assert!(matches!(checks[0].status, Status::Fail(_)));

        cfg.qbittorrent = Some(QBittorrent {
            url: "http://127.0.0.1:8080".into(),
            username: "x".into(),
            password_env: "X".into(),
        });
        let checks = check_urls(&cfg);
        assert!(matches!(checks[0].status, Status::Ok(_)));
    }

    #[test]
    fn env_vars_must_be_set() {
        let root = tmp();
        let mut cfg = base_cfg(&root);
        let key = format!(
            "TQL_VALIDATE_TEST_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        cfg.qbittorrent = Some(QBittorrent {
            url: "http://127.0.0.1:8080".into(),
            username: "u".into(),
            password_env: key.clone(),
        });
        let checks = check_env_vars(&cfg);
        assert!(matches!(checks[0].status, Status::Fail(_)));

        // SAFETY: single-threaded test, env var unique per run.
        std::env::set_var(&key, "secret");
        let checks = check_env_vars(&cfg);
        assert!(matches!(checks[0].status, Status::Ok(_)));
        std::env::remove_var(&key);
    }

    #[test]
    fn env_check_never_leaks_value() {
        let root = tmp();
        let mut cfg = base_cfg(&root);
        let key = "TQL_VALIDATE_LEAK_TEST";
        let secret = "PLAINTEXT_SECRET_DO_NOT_LEAK";
        std::env::set_var(key, secret);
        cfg.qbittorrent = Some(QBittorrent {
            url: "http://127.0.0.1:8080".into(),
            username: "u".into(),
            password_env: key.into(),
        });
        cfg.notify.telegram = Some(Telegram {
            bot_token_env: key.into(),
            chat_id: "1".into(),
            parse_mode: "HTML".into(),
        });
        cfg.media.plex = Some(Plex {
            url: "http://plex".into(),
            token_env: key.into(),
            section_ids: vec![],
        });
        cfg.media.jellyfin = Some(Jellyfin {
            url: "http://jelly".into(),
            api_key_env: key.into(),
        });
        let checks = check_env_vars(&cfg);
        let mut buf: Vec<u8> = Vec::new();
        finish(checks, false, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(!out.contains(secret), "value leaked into output:\n{out}");
        std::env::remove_var(key);
    }

    #[test]
    fn mcp_http_requires_api_key_env() {
        let root = tmp();
        let mut cfg = base_cfg(&root);
        cfg.mcp.transport = McpTransport::Http;
        cfg.mcp.api_key_env = None;
        let checks = check_mcp_http(&cfg);
        assert!(matches!(checks[0].status, Status::Fail(_)));

        cfg.mcp.api_key_env = Some("TQL_MCP_KEY".into());
        let checks = check_mcp_http(&cfg);
        assert!(matches!(checks[0].status, Status::Ok(_)));
    }

    #[test]
    fn trackers_credentials_without_manifest_fail() {
        let root = tmp();
        let mut cfg = base_cfg(&root);
        cfg.trackers.insert(
            "ghost".into(),
            TrackerCreds {
                cookie_env: Some("X".into()),
                auth_header_env: None,
            },
        );
        let checks = check_trackers_static(&cfg);
        assert!(checks
            .iter()
            .any(|c| c.name == "trackers.ghost"
                && matches!(c.status, Status::Fail(ref m) if m.contains("no manifest"))));
    }

    #[test]
    fn json_output_uses_doctor_shape_with_exit_code() {
        let root = tmp();
        let cfg = base_cfg(&root);
        let mut checks = vec![Check {
            name: "config".into(),
            status: Status::Ok("parsed".into()),
        }];
        checks.extend(check_paths(&cfg));
        let mut buf: Vec<u8> = Vec::new();
        finish(checks, true, &mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v["exit_code"], 0);
        assert!(v["checks"].is_array());
        assert_eq!(v["summary"]["fail"], 0);
    }
}
