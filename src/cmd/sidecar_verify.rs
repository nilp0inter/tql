//! `tql sidecar verify` — cross-check every sidecar against the filesystem.
//!
//! For each sidecar in `<library_root>/.metadata/`:
//!   - load it (unreadable / malformed → reported as a `read_error` issue);
//!   - check `content_path` exists (the seed source the sidecar was built
//!     from); if missing, all per-site inode checks degrade to existence-only;
//!   - for every `link_sites[i].resolved_path`: check it exists; if
//!     `is_directory == false`, also assert its inode matches `content_path`'s
//!     inode (the hardlink invariant from §9).
//!
//! Exits 1 if any sidecar reports at least one issue, 0 otherwise. `--json`
//! emits a structured report instead of the line-oriented summary.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde_json::{json, Value as Json};

use crate::config::{self, Config};
use crate::sidecar::{self, Sidecar};

#[derive(Parser, Debug)]
pub struct Args {
    /// Emit a JSON report instead of the human-readable table.
    #[arg(long)]
    pub json: bool,
    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<(), u8> {
    let (_path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sidecar verify: {e}");
            return Err(1);
        }
    };
    verify(&cfg, args.json, &mut std::io::stdout())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Issue {
    ReadError(String),
    MissingContent { path: String },
    MissingResolved { site: String, path: String },
    InodeMismatch { site: String, path: String },
}

impl Issue {
    fn kind(&self) -> &'static str {
        match self {
            Issue::ReadError(_) => "read_error",
            Issue::MissingContent { .. } => "missing_content",
            Issue::MissingResolved { .. } => "missing_resolved",
            Issue::InodeMismatch { .. } => "inode_mismatch",
        }
    }

    fn to_json(&self) -> Json {
        match self {
            Issue::ReadError(msg) => json!({ "kind": self.kind(), "message": msg }),
            Issue::MissingContent { path } => json!({ "kind": self.kind(), "path": path }),
            Issue::MissingResolved { site, path } => {
                json!({ "kind": self.kind(), "site": site, "path": path })
            }
            Issue::InodeMismatch { site, path } => {
                json!({ "kind": self.kind(), "site": site, "path": path })
            }
        }
    }

    fn render_line(&self, hash: &str) -> String {
        match self {
            Issue::ReadError(msg) => format!("{hash}  read_error  {msg}"),
            Issue::MissingContent { path } => format!("{hash}  missing_content  {path}"),
            Issue::MissingResolved { site, path } => {
                format!("{hash}  missing_resolved  {site} -> {path}")
            }
            Issue::InodeMismatch { site, path } => {
                format!("{hash}  inode_mismatch  {site} -> {path}")
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub info_hash_v1: String,
    pub issues: Vec<Issue>,
}

pub(crate) fn verify<W: std::io::Write>(cfg: &Config, json: bool, out: &mut W) -> Result<(), u8> {
    let entries = match scan(&cfg.paths.library_root) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sidecar verify: {e}");
            return Err(1);
        }
    };

    let scanned = entries.len();
    let ok = entries.iter().filter(|e| e.issues.is_empty()).count();
    let with_issues = scanned - ok;
    let issue_total: usize = entries.iter().map(|e| e.issues.len()).sum();

    if json {
        let arr: Vec<Json> = entries
            .iter()
            .map(|e| {
                json!({
                    "info_hash_v1": e.info_hash_v1,
                    "ok": e.issues.is_empty(),
                    "issues": e.issues.iter().map(Issue::to_json).collect::<Vec<_>>(),
                })
            })
            .collect();
        let doc = json!({
            "entries": arr,
            "summary": {
                "scanned": scanned,
                "ok": ok,
                "with_issues": with_issues,
                "issues_total": issue_total,
            },
        });
        match serde_json::to_string_pretty(&doc) {
            Ok(s) => {
                let _ = writeln!(out, "{s}");
            }
            Err(e) => {
                eprintln!("sidecar verify: serialize: {e}");
                return Err(1);
            }
        }
    } else {
        for entry in &entries {
            if entry.issues.is_empty() {
                let _ = writeln!(out, "{}  ok", entry.info_hash_v1);
            } else {
                for issue in &entry.issues {
                    let _ = writeln!(out, "{}", issue.render_line(&entry.info_hash_v1));
                }
            }
        }
        let _ = writeln!(
            out,
            "scanned={scanned} ok={ok} with_issues={with_issues} issues_total={issue_total}"
        );
    }

    if with_issues > 0 {
        Err(1)
    } else {
        Ok(())
    }
}

pub(crate) fn scan(library_root: &Path) -> Result<Vec<Entry>, String> {
    let meta_dir = library_root.join(".metadata");
    let entries = match fs::read_dir(&meta_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", meta_dir.display())),
    };

    let mut paths: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Some(hash) = name.strip_suffix(".json") else {
            continue;
        };
        paths.push((hash.to_lowercase(), entry.path()));
    }
    paths.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = Vec::with_capacity(paths.len());
    for (hash, path) in paths {
        out.push(verify_one(hash, &path));
    }
    Ok(out)
}

fn verify_one(hash: String, path: &Path) -> Entry {
    let sc = match sidecar::read(path) {
        Ok(Some(sc)) => sc,
        Ok(None) => {
            // Should not happen (read_dir saw it), but treat as a read error.
            return Entry {
                info_hash_v1: hash,
                issues: vec![Issue::ReadError("sidecar disappeared".into())],
            };
        }
        Err(e) => {
            return Entry {
                info_hash_v1: hash,
                issues: vec![Issue::ReadError(e.to_string())],
            };
        }
    };
    let issues = check_sidecar(&sc);
    Entry {
        info_hash_v1: sc.info_hash_v1,
        issues,
    }
}

fn check_sidecar(sc: &Sidecar) -> Vec<Issue> {
    let mut issues = Vec::new();

    let content_meta = fs::metadata(&sc.content_path).ok();
    if content_meta.is_none() {
        issues.push(Issue::MissingContent {
            path: sc.content_path.clone(),
        });
    }

    for site in &sc.link_sites {
        match fs::metadata(&site.resolved_path) {
            Err(_) => issues.push(Issue::MissingResolved {
                site: site.relative_path.clone(),
                path: site.resolved_path.clone(),
            }),
            Ok(site_meta) => {
                // Inode equality is only meaningful for single-file torrents.
                // Directories replicate structure, not inodes.
                if !sc.is_directory {
                    if let Some(cm) = &content_meta {
                        if cm.dev() != site_meta.dev() || cm.ino() != site_meta.ino() {
                            issues.push(Issue::InodeMismatch {
                                site: site.relative_path.clone(),
                                path: site.resolved_path.clone(),
                            });
                        }
                    }
                }
            }
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::{
        Api, Linking, Mcp, Media, Notify, Paths, QBittorrent, Reconcile, Scripting,
    };
    use crate::sidecar::{LinkSite, Origin, Sidecar, SCHEMA_VERSION};

    fn tmp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nano = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        p.push(format!("tql-verify-{tag}-{pid}-{nano}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn cfg_with_lib(library_root: PathBuf) -> Config {
        Config {
            paths: Paths {
                seed_root: library_root.clone(),
                library_root,
                trackers_root: PathBuf::from("/nonexistent"),
            },
            linking: Linking::default(),
            qbittorrent: None::<QBittorrent>,
            notify: Notify::default(),
            media: Media::default(),
            reconcile: Reconcile::default(),
            mcp: Mcp::default(),
            api: Api::default(),
            scripting: Scripting::default(),
            trackers: Default::default(),
        }
    }

    fn write_sidecar(lib: &Path, sc: &Sidecar) {
        let p = sidecar::sidecar_path(lib, &sc.info_hash_v1);
        sidecar::write(&p, sc).unwrap();
    }

    fn mk_file_sidecar(hash: &str, content: &Path, sites: Vec<(&str, &Path)>) -> Sidecar {
        let size = std::fs::metadata(content).map(|m| m.len()).unwrap_or(0);
        Sidecar {
            schema_version: SCHEMA_VERSION,
            info_hash_v1: hash.into(),
            info_hash_v2: None,
            name: "Book".into(),
            category: "demo.org".into(),
            content_path: content.to_string_lossy().into_owned(),
            is_directory: false,
            size_bytes: size,
            link_sites: sites
                .into_iter()
                .map(|(rel, p)| LinkSite {
                    relative_path: rel.into(),
                    resolved_path: p.to_string_lossy().into_owned(),
                    created_at: "2026-05-11T00:00:00Z".into(),
                    origin: Origin::PostProcess,
                })
                .collect(),
            last_applied_tags: vec![],
            last_applied_at: None,
            warnings: vec![],
        }
    }

    #[test]
    fn verify_empty_metadata_dir_is_ok() {
        let lib = tmp_dir("empty");
        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = verify(&cfg, false, &mut buf);
        assert!(r.is_ok());
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("scanned=0"));
    }

    #[test]
    fn verify_passes_when_link_targets_share_inode_with_content() {
        let lib = tmp_dir("ok");
        // Real seed + hardlinked site.
        let seed = lib.join("seed.bin");
        std::fs::write(&seed, b"hello").unwrap();
        let site_dir = lib.join("demo.org").join("Cat");
        std::fs::create_dir_all(&site_dir).unwrap();
        let site = site_dir.join("Book");
        std::fs::hard_link(&seed, &site).unwrap();

        let sc = mk_file_sidecar("aaa", &seed, vec![("Cat", &site)]);
        write_sidecar(&lib, &sc);

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = verify(&cfg, false, &mut buf);
        assert!(r.is_ok());
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("aaa  ok"));
        assert!(s.contains("scanned=1 ok=1"));
    }

    #[test]
    fn verify_flags_missing_resolved_path() {
        let lib = tmp_dir("missing");
        let seed = lib.join("seed.bin");
        std::fs::write(&seed, b"hello").unwrap();
        let nonexistent = lib.join("demo.org").join("Cat").join("Book");
        let sc = mk_file_sidecar("bbb", &seed, vec![("Cat", &nonexistent)]);
        write_sidecar(&lib, &sc);

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = verify(&cfg, false, &mut buf);
        assert_eq!(r, Err(1));
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("missing_resolved"));
        assert!(s.contains("with_issues=1"));
    }

    #[test]
    fn verify_flags_inode_mismatch_when_site_is_independent_copy() {
        let lib = tmp_dir("inode");
        let seed = lib.join("seed.bin");
        std::fs::write(&seed, b"hello").unwrap();
        let site_dir = lib.join("demo.org").join("Cat");
        std::fs::create_dir_all(&site_dir).unwrap();
        let site = site_dir.join("Book");
        // NOT hardlinked — separate file with same content => different inode.
        std::fs::write(&site, b"hello").unwrap();

        let sc = mk_file_sidecar("ccc", &seed, vec![("Cat", &site)]);
        write_sidecar(&lib, &sc);

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = verify(&cfg, false, &mut buf);
        assert_eq!(r, Err(1));
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("inode_mismatch"));
    }

    #[test]
    fn verify_flags_missing_content() {
        let lib = tmp_dir("nocontent");
        let seed = lib.join("seed.bin"); // never created
        let site_dir = lib.join("demo.org").join("Cat");
        std::fs::create_dir_all(&site_dir).unwrap();
        let site = site_dir.join("Book");
        std::fs::write(&site, b"hello").unwrap();

        let sc = mk_file_sidecar("ddd", &seed, vec![("Cat", &site)]);
        write_sidecar(&lib, &sc);

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = verify(&cfg, false, &mut buf);
        assert_eq!(r, Err(1));
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("missing_content"));
        // Site exists, content missing — we should not also flag inode_mismatch
        // because we have no content inode to compare against.
        assert!(!s.contains("inode_mismatch"));
    }

    #[test]
    fn verify_json_shape_matches_spec() {
        let lib = tmp_dir("json");
        let seed = lib.join("seed.bin");
        std::fs::write(&seed, b"hello").unwrap();
        let missing = lib.join("demo.org").join("Cat").join("Book");
        let sc = mk_file_sidecar("eee", &seed, vec![("Cat", &missing)]);
        write_sidecar(&lib, &sc);

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = verify(&cfg, true, &mut buf);
        assert_eq!(r, Err(1));
        let parsed: Json = serde_json::from_slice(&buf).unwrap();
        let entries = parsed["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["info_hash_v1"], "eee");
        assert_eq!(entries[0]["ok"], false);
        let issues = entries[0]["issues"].as_array().unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0]["kind"], "missing_resolved");
        assert_eq!(parsed["summary"]["scanned"], 1);
        assert_eq!(parsed["summary"]["with_issues"], 1);
        assert_eq!(parsed["summary"]["ok"], 0);
    }

    #[test]
    fn verify_malformed_sidecar_is_read_error_not_panic() {
        let lib = tmp_dir("malformed");
        let meta = lib.join(".metadata");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("deadbeef.json"), b"not json").unwrap();

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = verify(&cfg, false, &mut buf);
        assert_eq!(r, Err(1));
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("read_error"));
        assert!(s.contains("deadbeef"));
    }

    #[test]
    fn verify_directory_sidecar_existence_only() {
        // For directory torrents, we only check resolved_path *exists*, no inode check.
        let lib = tmp_dir("dir");
        let seed_dir = lib.join("seed-bundle");
        std::fs::create_dir_all(&seed_dir).unwrap();
        let site_dir = lib.join("demo.org").join("Cat").join("Bundle");
        std::fs::create_dir_all(&site_dir).unwrap();

        let sc = Sidecar {
            schema_version: SCHEMA_VERSION,
            info_hash_v1: "fff".into(),
            info_hash_v2: None,
            name: "Bundle".into(),
            category: "demo.org".into(),
            content_path: seed_dir.to_string_lossy().into_owned(),
            is_directory: true,
            size_bytes: 0,
            link_sites: vec![LinkSite {
                relative_path: "Cat".into(),
                resolved_path: site_dir.to_string_lossy().into_owned(),
                created_at: "2026-05-11T00:00:00Z".into(),
                origin: Origin::PostProcess,
            }],
            last_applied_tags: vec![],
            last_applied_at: None,
            warnings: vec![],
        };
        write_sidecar(&lib, &sc);

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = verify(&cfg, false, &mut buf);
        assert!(r.is_ok(), "got: {}", String::from_utf8_lossy(&buf));
    }
}
