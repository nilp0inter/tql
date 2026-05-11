//! `tql sidecar list` — enumerate every sidecar under
//! `<library_root>/.metadata/` with a brief per-entry summary.
//!
//! Plain output is one line per sidecar, sorted by hash:
//! `<hash>  <category>  <sites>  <name>`
//!
//! `--json` emits an array of `{info_hash_v1, category, name, sites_count,
//! size_bytes, is_directory}` objects, sorted by hash. Unreadable sidecars are
//! reported to stderr and skipped; the command still exits 0 unless the
//! `.metadata/` directory itself is unreadable (other than NotFound).

use std::fs;
use std::path::PathBuf;

use clap::Parser;
use serde_json::{json, Value as Json};

use crate::config::{self, Config};
use crate::sidecar;

#[derive(Parser, Debug)]
pub struct Args {
    /// Emit a JSON array instead of the human-readable table.
    #[arg(long)]
    pub json: bool,
    /// Restrict the listing to sidecars whose `category` matches (case-insensitive).
    #[arg(long, value_name = "CAT")]
    pub category: Option<String>,
    /// Explicit config file path; overrides the default search.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<(), u8> {
    let (_path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sidecar list: {e}");
            return Err(1);
        }
    };
    list(
        &cfg,
        args.json,
        args.category.as_deref(),
        &mut std::io::stdout(),
    )
}

#[derive(Debug, Clone)]
pub(crate) struct Summary {
    pub info_hash_v1: String,
    pub category: String,
    pub name: String,
    pub sites_count: usize,
    pub size_bytes: u64,
    pub is_directory: bool,
}

pub(crate) fn list<W: std::io::Write>(
    cfg: &Config,
    json: bool,
    category_filter: Option<&str>,
    out: &mut W,
) -> Result<(), u8> {
    let mut summaries = match collect(&cfg.paths.library_root) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sidecar list: {e}");
            return Err(1);
        }
    };

    if let Some(cat) = category_filter {
        let needle = cat.to_lowercase();
        summaries.retain(|s| s.category.to_lowercase() == needle);
    }

    if json {
        let arr: Vec<Json> = summaries
            .iter()
            .map(|s| {
                json!({
                    "info_hash_v1": s.info_hash_v1,
                    "category": s.category,
                    "name": s.name,
                    "sites_count": s.sites_count,
                    "size_bytes": s.size_bytes,
                    "is_directory": s.is_directory,
                })
            })
            .collect();
        match serde_json::to_string_pretty(&Json::Array(arr)) {
            Ok(s) => {
                let _ = writeln!(out, "{s}");
            }
            Err(e) => {
                eprintln!("sidecar list: serialize: {e}");
                return Err(1);
            }
        }
    } else {
        for s in &summaries {
            let _ = writeln!(
                out,
                "{}  {}  {} sites  {}",
                s.info_hash_v1, s.category, s.sites_count, s.name
            );
        }
    }
    Ok(())
}

pub(crate) fn collect(library_root: &std::path::Path) -> Result<Vec<Summary>, String> {
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
        match sidecar::read(&path) {
            Ok(Some(sc)) => out.push(Summary {
                info_hash_v1: sc.info_hash_v1,
                category: sc.category,
                name: sc.name,
                sites_count: sc.link_sites.len(),
                size_bytes: sc.size_bytes,
                is_directory: sc.is_directory,
            }),
            Ok(None) => {}
            Err(e) => {
                eprintln!("sidecar list: read {}: {e}", path.display());
                // Surface a synthetic entry so the operator can act on it.
                out.push(Summary {
                    info_hash_v1: hash,
                    category: String::new(),
                    name: String::new(),
                    sites_count: 0,
                    size_bytes: 0,
                    is_directory: false,
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
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
        p.push(format!("tql-list-{tag}-{pid}-{nano}"));
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

    fn sample(hash: &str, cat: &str, name: &str, sites: usize) -> Sidecar {
        let mut link_sites = Vec::new();
        for i in 0..sites {
            link_sites.push(LinkSite {
                relative_path: format!("S{i}"),
                resolved_path: format!("/lib/{cat}/S{i}/{name}"),
                created_at: "2026-05-11T00:00:00Z".into(),
                origin: Origin::PostProcess,
            });
        }
        Sidecar {
            schema_version: SCHEMA_VERSION,
            info_hash_v1: hash.to_string(),
            info_hash_v2: None,
            name: name.into(),
            category: cat.into(),
            content_path: format!("/seed/{cat}/{name}"),
            is_directory: false,
            size_bytes: 1234,
            link_sites,
            last_applied_tags: vec![],
            last_applied_at: None,
            warnings: vec![],
        }
    }

    #[test]
    fn list_empty_when_metadata_dir_missing() {
        let lib = tmp_dir("missing");
        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        list(&cfg, false, None, &mut buf).unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn list_plain_lists_sorted_by_hash() {
        let lib = tmp_dir("plain");
        for (h, c, n, s) in [
            ("ccc", "demo.org", "Three", 0),
            ("aaa", "demo.org", "One", 2),
            ("bbb", "other.tld", "Two", 1),
        ] {
            let p = sidecar::sidecar_path(&lib, h);
            sidecar::write(&p, &sample(h, c, n, s)).unwrap();
        }
        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        list(&cfg, false, None, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("aaa  demo.org  2 sites  One"));
        assert!(lines[1].starts_with("bbb  other.tld  1 sites  Two"));
        assert!(lines[2].starts_with("ccc  demo.org  0 sites  Three"));
    }

    #[test]
    fn list_json_shape() {
        let lib = tmp_dir("json");
        let p = sidecar::sidecar_path(&lib, "abc");
        sidecar::write(&p, &sample("abc", "demo.org", "Book", 1)).unwrap();
        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        list(&cfg, true, None, &mut buf).unwrap();
        let parsed: Json = serde_json::from_slice(&buf).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["info_hash_v1"], "abc");
        assert_eq!(arr[0]["category"], "demo.org");
        assert_eq!(arr[0]["name"], "Book");
        assert_eq!(arr[0]["sites_count"], 1);
        assert_eq!(arr[0]["size_bytes"], 1234);
        assert_eq!(arr[0]["is_directory"], false);
    }

    #[test]
    fn list_skips_dotfiles_and_non_json() {
        let lib = tmp_dir("filter");
        let p = sidecar::sidecar_path(&lib, "abc");
        sidecar::write(&p, &sample("abc", "demo.org", "Book", 0)).unwrap();
        // dotfile (lock-like) and a non-.json
        let meta = lib.join(".metadata");
        std::fs::write(meta.join(".abc.json.lock"), b"").unwrap();
        std::fs::write(meta.join("README.txt"), b"hi").unwrap();
        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        list(&cfg, true, None, &mut buf).unwrap();
        let parsed: Json = serde_json::from_slice(&buf).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 1);
    }

    #[test]
    fn list_category_filter_restricts_results_case_insensitively() {
        let lib = tmp_dir("catfilter");
        for (h, c, n) in [
            ("aaa", "demo.org", "One"),
            ("bbb", "other.tld", "Two"),
            ("ccc", "DEMO.ORG", "Three"),
        ] {
            let p = sidecar::sidecar_path(&lib, h);
            sidecar::write(&p, &sample(h, c, n, 0)).unwrap();
        }
        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        list(&cfg, true, Some("Demo.Org"), &mut buf).unwrap();
        let parsed: Json = serde_json::from_slice(&buf).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let hashes: Vec<&str> = arr
            .iter()
            .map(|v| v["info_hash_v1"].as_str().unwrap())
            .collect();
        assert_eq!(hashes, vec!["aaa", "ccc"]);
    }

    #[test]
    fn list_category_filter_with_no_match_returns_empty() {
        let lib = tmp_dir("catnomatch");
        let p = sidecar::sidecar_path(&lib, "abc");
        sidecar::write(&p, &sample("abc", "demo.org", "Book", 0)).unwrap();
        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        list(&cfg, true, Some("nope.tld"), &mut buf).unwrap();
        let parsed: Json = serde_json::from_slice(&buf).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 0);
    }

    #[test]
    fn list_malformed_sidecar_surfaces_as_stub_entry() {
        let lib = tmp_dir("bad");
        let meta = lib.join(".metadata");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("deadbeef.json"), b"not json").unwrap();
        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        list(&cfg, true, None, &mut buf).unwrap();
        let parsed: Json = serde_json::from_slice(&buf).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["info_hash_v1"], "deadbeef");
        assert_eq!(arr[0]["category"], "");
    }
}
