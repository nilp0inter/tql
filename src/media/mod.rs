//! Media-server refresh (DESIGN.md §15).
//!
//! Best-effort, fire-and-warn refresh of Plex and Jellyfin after the
//! post-processor mutates link sites. Both backends use a 5 s per-request
//! timeout, no retry. Every failure is a warning, never an abort: the
//! library tree and sidecar are the source of truth.

#![allow(dead_code)]

pub mod jellyfin;
pub mod plex;

use std::path::{Path, PathBuf};

use crate::config::Config;

/// Per DESIGN.md §15.
pub const TIMEOUT_SECS: u64 = 5;

/// Refresh every configured media backend for the freshly-applied link sites.
///
/// `added_abs_paths` is the list of new absolute filesystem paths the
/// post-processor just created. Empty input → empty output (no-op).
pub async fn refresh_all(cfg: &Config, added_abs_paths: &[PathBuf]) -> Vec<String> {
    if added_abs_paths.is_empty() {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    if let Some(plex_cfg) = &cfg.media.plex {
        match resolve_env(&plex_cfg.token_env) {
            Some(token) => {
                if let Err(msgs) = plex::refresh(plex_cfg, &token, added_abs_paths).await {
                    warnings.extend(msgs);
                }
            }
            None => warnings.push(format!(
                "media plex: token env {:?} not set; skipping",
                plex_cfg.token_env
            )),
        }
    }
    if let Some(jf_cfg) = &cfg.media.jellyfin {
        match resolve_env(&jf_cfg.api_key_env) {
            Some(key) => {
                if let Err(msg) = jellyfin::refresh(jf_cfg, &key, added_abs_paths).await {
                    warnings.push(msg);
                }
            }
            None => warnings.push(format!(
                "media jellyfin: api_key env {:?} not set; skipping",
                jf_cfg.api_key_env
            )),
        }
    }
    warnings
}

/// Sync wrapper for callers (post-process) that don't run a tokio runtime.
///
/// Spins up a private current-thread runtime per call. Cheap relative to the
/// per-torrent post-process pipeline; only invoked when there are changes.
pub fn refresh_blocking(cfg: &Config, added_abs_paths: &[PathBuf]) -> Vec<String> {
    if added_abs_paths.is_empty() {
        return Vec::new();
    }
    if cfg.media.plex.is_none() && cfg.media.jellyfin.is_none() {
        return Vec::new();
    }
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return vec![format!("media refresh: tokio runtime: {e}")],
    };
    rt.block_on(refresh_all(cfg, added_abs_paths))
}

fn resolve_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Absolute paths for a freshly-applied list of link-site relative paths.
/// Helper for the post-processor.
pub fn site_abs_paths(
    library_root: &Path,
    category: &str,
    name: &str,
    rel_paths: &[String],
) -> Vec<PathBuf> {
    rel_paths
        .iter()
        .map(|rel| library_root.join(category).join(rel).join(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Plex;

    #[test]
    fn site_abs_paths_joins_components() {
        let root = Path::new("/lib");
        let paths = site_abs_paths(root, "t.tld", "Album", &["A".into(), "B/C".into()]);
        assert_eq!(paths[0], PathBuf::from("/lib/t.tld/A/Album"));
        assert_eq!(paths[1], PathBuf::from("/lib/t.tld/B/C/Album"));
    }

    #[test]
    fn refresh_blocking_noop_when_nothing_configured() {
        let cfg = Config {
            paths: crate::config::Paths {
                seed_root: "/s".into(),
                library_root: "/l".into(),
                trackers_root: "/t".into(),
            },
            linking: Default::default(),
            qbittorrent: None,
            notify: Default::default(),
            media: Default::default(),
            reconcile: Default::default(),
            mcp: Default::default(),
            api: Default::default(),
            scripting: Default::default(),
            trackers: Default::default(),
        };
        let out = refresh_blocking(&cfg, &[PathBuf::from("/l/x")]);
        assert!(out.is_empty());
    }

    #[test]
    fn refresh_blocking_empty_paths_is_noop() {
        // Even with a backend configured, no paths → no warnings.
        let cfg = Config {
            paths: crate::config::Paths {
                seed_root: "/s".into(),
                library_root: "/l".into(),
                trackers_root: "/t".into(),
            },
            linking: Default::default(),
            qbittorrent: None,
            notify: Default::default(),
            media: crate::config::Media {
                plex: Some(Plex {
                    url: "http://127.0.0.1:1".into(),
                    token_env: "DOES_NOT_EXIST_ZZZ".into(),
                    section_ids: vec![1],
                }),
                jellyfin: None,
            },
            reconcile: Default::default(),
            mcp: Default::default(),
            api: Default::default(),
            scripting: Default::default(),
            trackers: Default::default(),
        };
        let out = refresh_blocking(&cfg, &[]);
        assert!(out.is_empty());
    }
}
