//! Tracing subscriber wiring per DESIGN.md §6.
//!
//! Human format → stderr, optional JSONL → `$TQL_LOG_FILE`. Level/filter
//! controlled by `TQL_LOG` (defaults to `info`) using the standard
//! `tracing-subscriber` `EnvFilter` syntax.
//!
//! Both knobs are env-only so the config surface stays unchanged; a
//! `[logging]` TOML block is a future leg if it becomes necessary.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

const DEFAULT_FILTER: &str = "info";

/// Resolve the filter string. Env wins over the default.
pub(crate) fn resolve_filter() -> String {
    std::env::var("TQL_LOG").unwrap_or_else(|_| DEFAULT_FILTER.into())
}

/// Resolve the optional JSONL log file path from `$TQL_LOG_FILE`.
/// Empty string is treated as unset.
pub(crate) fn resolve_log_file() -> Option<PathBuf> {
    match std::env::var("TQL_LOG_FILE") {
        Ok(p) if !p.is_empty() => Some(PathBuf::from(p)),
        _ => None,
    }
}

fn open_log_file(path: &PathBuf) -> Option<File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Install the global subscriber. Idempotent — repeated calls (e.g. from
/// tests) silently no-op via `try_init`.
pub fn init() {
    let filter = EnvFilter::try_new(resolve_filter()).unwrap_or_else(|_| EnvFilter::new("info"));

    let stderr_layer = fmt::layer().with_writer(std::io::stderr).with_target(false);

    let file_layer = resolve_log_file()
        .as_ref()
        .and_then(open_log_file)
        .map(|file| fmt::layer().json().with_writer(std::sync::Mutex::new(file)));

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var manipulation must be serialized to avoid races between
    // parallel tests touching the same names.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_env<F: FnOnce()>(vars: &[(&str, Option<&str>)], f: F) {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(String, Option<String>)> = vars
            .iter()
            .map(|(k, _)| (k.to_string(), std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
        f();
        for (k, v) in saved {
            match v {
                Some(val) => std::env::set_var(&k, val),
                None => std::env::remove_var(&k),
            }
        }
    }

    #[test]
    fn resolve_filter_defaults_to_info_when_env_unset() {
        with_env(&[("TQL_LOG", None)], || {
            assert_eq!(resolve_filter(), "info");
        });
    }

    #[test]
    fn resolve_filter_honors_env_override() {
        with_env(&[("TQL_LOG", Some("debug,tql::qbit=trace"))], || {
            assert_eq!(resolve_filter(), "debug,tql::qbit=trace");
        });
    }

    #[test]
    fn resolve_log_file_returns_none_when_unset_or_empty() {
        with_env(&[("TQL_LOG_FILE", None)], || {
            assert!(resolve_log_file().is_none());
        });
        with_env(&[("TQL_LOG_FILE", Some(""))], || {
            assert!(resolve_log_file().is_none());
        });
    }

    #[test]
    fn resolve_log_file_returns_path_when_set() {
        with_env(&[("TQL_LOG_FILE", Some("/tmp/tql.jsonl"))], || {
            assert_eq!(resolve_log_file(), Some(PathBuf::from("/tmp/tql.jsonl")));
        });
    }

    #[test]
    fn open_log_file_creates_parent_dirs() {
        let tmp = std::env::temp_dir().join(format!(
            "tql-logging-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = tmp.join("sub").join("dir").join("log.jsonl");
        let opened = open_log_file(&target);
        assert!(opened.is_some(), "log file should open");
        assert!(target.exists(), "log file should be on disk");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn init_is_idempotent() {
        // Should never panic even when called repeatedly; the second call
        // is a no-op because a global subscriber is already installed.
        init();
        init();
    }
}
