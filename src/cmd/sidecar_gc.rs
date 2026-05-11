//! `tql sidecar gc` — orphan sidecar garbage collector (DESIGN.md §17).
//!
//! Walks `<library_root>/.metadata/*.json`, compares each sidecar's
//! `info_hash_v1` against the live qBittorrent torrent set, and removes the
//! orphans (sidecar + its link sites). Recommended cadence is manual or via
//! cron — not automatic.
//!
//! Exit code: 1 on connection failure or per-orphan errors, 0 otherwise.

use std::fs;
use std::path::PathBuf;

use clap::Parser;

use crate::config::{self, Config};
use crate::linking;
use crate::qbit;
use crate::qbit::types::TorrentsInfoQuery;
use crate::sidecar;

#[derive(Parser, Debug)]
pub struct Args {
    /// Print intended deletions but do not touch the filesystem.
    #[arg(long)]
    pub dry_run: bool,
    /// Emit one JSON document instead of human-readable output.
    #[arg(long)]
    pub json: bool,
    /// Restrict the scan to a single sidecar by info_hash_v1 (case-insensitive).
    #[arg(long, value_name = "HASH")]
    pub hash: Option<String>,
    /// Restrict the scan to sidecars whose category matches (case-insensitive).
    #[arg(long, value_name = "CAT")]
    pub category: Option<String>,
    /// Optional config-file override.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub scanned: usize,
    pub kept: usize,
    pub orphans: usize,
    pub removed: usize,
    pub sites_unlinked: usize,
    pub errors: usize,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub info_hash_v1: String,
    pub status: EntryStatus,
    pub removed: bool,
    pub sites_unlinked: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryStatus {
    Kept,
    Orphan,
    ReadError,
}

#[derive(Debug, Default, Clone)]
pub struct Report {
    pub summary: Summary,
    pub entries: Vec<Entry>,
}

impl Summary {
    fn print(&self) {
        eprintln!(
            "tql sidecar gc: scanned {}, kept {}, orphans {}, removed {}, sites unlinked {}, errors {}",
            self.scanned, self.kept, self.orphans, self.removed, self.sites_unlinked, self.errors
        );
    }
}

impl EntryStatus {
    fn as_str(&self) -> &'static str {
        match self {
            EntryStatus::Kept => "kept",
            EntryStatus::Orphan => "orphan",
            EntryStatus::ReadError => "read_error",
        }
    }
}

fn render_json(report: &Report, dry_run: bool) -> String {
    use serde_json::json;
    let entries: Vec<_> = report
        .entries
        .iter()
        .map(|e| {
            json!({
                "info_hash_v1": e.info_hash_v1,
                "status": e.status.as_str(),
                "removed": e.removed,
                "sites_unlinked": e.sites_unlinked,
                "errors": e.errors,
            })
        })
        .collect();
    let v = json!({
        "dry_run": dry_run,
        "entries": entries,
        "summary": {
            "scanned": report.summary.scanned,
            "kept": report.summary.kept,
            "orphans": report.summary.orphans,
            "removed": report.summary.removed,
            "sites_unlinked": report.summary.sites_unlinked,
            "errors": report.summary.errors,
        },
    });
    serde_json::to_string_pretty(&v).unwrap()
}

pub fn run(args: Args) -> Result<(), u8> {
    match do_run(&args) {
        Ok(report) => {
            if args.json {
                println!("{}", render_json(&report, args.dry_run));
            } else {
                report.summary.print();
            }
            if report.summary.errors > 0 {
                Err(1)
            } else {
                Ok(())
            }
        }
        Err(msg) => {
            if args.json {
                let v = serde_json::json!({"error": msg});
                println!("{}", serde_json::to_string_pretty(&v).unwrap());
            } else {
                eprintln!("tql sidecar gc: {msg}");
            }
            Err(1)
        }
    }
}

fn do_run(args: &Args) -> Result<Report, String> {
    let (_, cfg) = config::load(args.config.as_deref()).map_err(|e| format!("config: {e}"))?;
    let known = fetch_known_hashes(&cfg)?;
    Ok(gc_with_known_detailed(
        &cfg,
        &known,
        args.dry_run,
        args.json,
        args.hash.as_deref(),
        args.category.as_deref(),
    ))
}

fn fetch_known_hashes(cfg: &Config) -> Result<std::collections::BTreeSet<String>, String> {
    let qb = cfg
        .qbittorrent
        .as_ref()
        .ok_or_else(|| "config has no [qbittorrent] section".to_string())?;
    let password = std::env::var(&qb.password_env)
        .map_err(|_| format!("env var {} is not set", qb.password_env))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    runtime.block_on(async {
        let client = qbit::Client::new(&qb.url).map_err(|e| format!("qbit client: {e}"))?;
        client
            .login(&qb.username, &password)
            .await
            .map_err(|e| format!("qbit login: {e}"))?;
        let torrents = client
            .torrents_info(&TorrentsInfoQuery::default())
            .await
            .map_err(|e| format!("qbit torrents_info: {e}"))?;
        Ok(torrents
            .into_iter()
            .map(|t| t.hash.to_lowercase())
            .collect())
    })
}

fn gc_with_known_detailed(
    cfg: &Config,
    known: &std::collections::BTreeSet<String>,
    dry_run: bool,
    quiet: bool,
    hash_filter: Option<&str>,
    category_filter: Option<&str>,
) -> Report {
    let mut report = Report::default();
    let meta_dir = cfg.paths.library_root.join(".metadata");
    let entries = match fs::read_dir(&meta_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return report,
        Err(e) => {
            if !quiet {
                eprintln!("tql sidecar gc: read {}: {e}", meta_dir.display());
            }
            report.summary.errors += 1;
            return report;
        }
    };

    let mut sidecars: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Skip lock files (".<hash>.json.lock") and any other dotfile.
        if name.starts_with('.') {
            continue;
        }
        let Some(hash) = name.strip_suffix(".json") else {
            continue;
        };
        sidecars.push((hash.to_lowercase(), entry.path()));
    }
    sidecars.sort_by(|a, b| a.0.cmp(&b.0));

    if let Some(h) = hash_filter {
        let needle = h.to_lowercase();
        sidecars.retain(|(hash, _)| hash == &needle);
        if sidecars.is_empty() {
            if !quiet {
                eprintln!("tql sidecar gc: no sidecar matches hash {h}");
            }
            report.summary.errors += 1;
            return report;
        }
    }
    if let Some(c) = category_filter {
        let needle = c.to_lowercase();
        sidecars.retain(|(_, path)| match sidecar::read(path) {
            Ok(Some(sc)) => sc.category.to_lowercase() == needle,
            _ => false,
        });
        if sidecars.is_empty() {
            if !quiet {
                eprintln!("tql sidecar gc: no sidecar matches category {c}");
            }
            report.summary.errors += 1;
            return report;
        }
    }

    for (hash, path) in sidecars {
        report.summary.scanned += 1;
        if known.contains(&hash) {
            report.summary.kept += 1;
            report.entries.push(Entry {
                info_hash_v1: hash,
                status: EntryStatus::Kept,
                removed: false,
                sites_unlinked: 0,
                errors: vec![],
            });
            continue;
        }
        report.summary.orphans += 1;

        let mut entry = Entry {
            info_hash_v1: hash.clone(),
            status: EntryStatus::Orphan,
            removed: false,
            sites_unlinked: 0,
            errors: vec![],
        };

        let sc = match sidecar::read(&path) {
            Ok(Some(sc)) => sc,
            Ok(None) => {
                report.entries.push(entry);
                continue;
            }
            Err(e) => {
                let msg = format!("read {}: {e}", path.display());
                if !quiet {
                    eprintln!("tql sidecar gc: {msg}");
                }
                report.summary.errors += 1;
                entry.status = EntryStatus::ReadError;
                entry.errors.push(msg);
                report.entries.push(entry);
                continue;
            }
        };

        let cat_root = cfg.paths.library_root.join(&sc.category);
        let mut had_err = false;
        for site in &sc.link_sites {
            let target = PathBuf::from(&site.resolved_path);
            if dry_run {
                if !quiet {
                    println!("would unlink {}", target.display());
                }
                report.summary.sites_unlinked += 1;
                entry.sites_unlinked += 1;
                continue;
            }
            match linking::unlink_site(&target, &cat_root) {
                Ok(()) => {
                    report.summary.sites_unlinked += 1;
                    entry.sites_unlinked += 1;
                }
                Err(e) => {
                    let msg = format!("unlink {}: {e}", target.display());
                    if !quiet {
                        eprintln!("tql sidecar gc: {msg}");
                    }
                    report.summary.errors += 1;
                    entry.errors.push(msg);
                    had_err = true;
                }
            }
        }

        if dry_run {
            if !quiet {
                println!("would remove sidecar {}", path.display());
            }
            report.entries.push(entry);
            continue;
        }
        if had_err {
            // Leave the sidecar in place so a re-run can retry the remaining
            // sites; deleting it would forget the resolved paths.
            report.entries.push(entry);
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => {
                report.summary.removed += 1;
                entry.removed = true;
            }
            Err(e) => {
                let msg = format!("remove {}: {e}", path.display());
                if !quiet {
                    eprintln!("tql sidecar gc: {msg}");
                }
                report.summary.errors += 1;
                entry.errors.push(msg);
                report.entries.push(entry);
                continue;
            }
        }
        // Best-effort: also drop the adjacent lock file.
        let lock = path.with_file_name(format!(
            ".{}.lock",
            path.file_name().unwrap().to_string_lossy()
        ));
        let _ = fs::remove_file(&lock);
        report.entries.push(entry);
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Api, Linking, Mcp, Media, Notify, Paths, QBittorrent, Reconcile, Scripting,
    };
    use crate::sidecar::{LinkSite, Origin, Sidecar, SCHEMA_VERSION};
    use std::collections::BTreeSet;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut d = std::env::temp_dir();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            d.push(format!("tql-gc-{}-{}-{}", tag, std::process::id(), nanos));
            fs::create_dir_all(&d).unwrap();
            Self(d)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn cfg_with(library_root: PathBuf) -> Config {
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

    fn seed_sidecar_with_site(lib: &std::path::Path, hash: &str, rel: &str) -> PathBuf {
        // Materialize a real linked file so unlink_site can remove it.
        let category = "tracker.tld";
        let cat_root = lib.join(category);
        let site_dir = cat_root.join(rel);
        fs::create_dir_all(&site_dir).unwrap();
        let target = site_dir.join("Book");
        fs::write(&target, b"epub").unwrap();

        let sc = Sidecar {
            schema_version: SCHEMA_VERSION,
            info_hash_v1: hash.into(),
            info_hash_v2: None,
            name: "Book".into(),
            category: category.into(),
            content_path: format!("{}/Book", lib.display()),
            is_directory: false,
            size_bytes: 4,
            link_sites: vec![LinkSite {
                relative_path: rel.into(),
                resolved_path: target.to_string_lossy().into_owned(),
                created_at: "2026-05-11T00:00:00Z".into(),
                origin: Origin::PostProcess,
            }],
            last_applied_tags: vec![format!("link:{rel}")],
            last_applied_at: Some("2026-05-11T00:00:00Z".into()),
            warnings: vec![],
        };
        let p = sidecar::sidecar_path(lib, hash);
        sidecar::write(&p, &sc).unwrap();
        target
    }

    #[test]
    fn orphan_removed_and_link_site_unlinked() {
        let d = TempDir::new("orphan");
        let lib = d.0.join("lib");
        fs::create_dir_all(&lib).unwrap();
        let target = seed_sidecar_with_site(&lib, "deadbeef", "Cat/Sub");
        let sc_path = sidecar::sidecar_path(&lib, "deadbeef");
        assert!(sc_path.is_file());
        assert!(target.is_file());

        let cfg = cfg_with(lib.clone());
        let known: BTreeSet<String> = BTreeSet::new(); // no known torrents → orphan
        let s = gc_with_known_detailed(&cfg, &known, false, true, None, None).summary;

        assert_eq!(s.scanned, 1);
        assert_eq!(s.kept, 0);
        assert_eq!(s.orphans, 1);
        assert_eq!(s.removed, 1);
        assert_eq!(s.sites_unlinked, 1);
        assert_eq!(s.errors, 0);
        assert!(!sc_path.exists(), "sidecar still present");
        assert!(!target.exists(), "link target still present");
        // Cat/Sub and Cat should be pruned up to the category boundary.
        assert!(!lib.join("tracker.tld/Cat").exists(), "parent not pruned");
    }

    #[test]
    fn known_hash_is_kept_untouched() {
        let d = TempDir::new("keep");
        let lib = d.0.join("lib");
        fs::create_dir_all(&lib).unwrap();
        let target = seed_sidecar_with_site(&lib, "cafebabe", "Cat/Sub");
        let sc_path = sidecar::sidecar_path(&lib, "cafebabe");

        let cfg = cfg_with(lib.clone());
        let mut known = BTreeSet::new();
        known.insert("cafebabe".into());
        let s = gc_with_known_detailed(&cfg, &known, false, true, None, None).summary;

        assert_eq!(s.scanned, 1);
        assert_eq!(s.kept, 1);
        assert_eq!(s.orphans, 0);
        assert_eq!(s.removed, 0);
        assert!(sc_path.is_file());
        assert!(target.is_file());
    }

    #[test]
    fn dry_run_leaves_fs_untouched() {
        let d = TempDir::new("dry");
        let lib = d.0.join("lib");
        fs::create_dir_all(&lib).unwrap();
        let target = seed_sidecar_with_site(&lib, "deadbeef", "Cat/Sub");
        let sc_path = sidecar::sidecar_path(&lib, "deadbeef");

        let cfg = cfg_with(lib.clone());
        let known = BTreeSet::new();
        let s = gc_with_known_detailed(&cfg, &known, true, true, None, None).summary;

        assert_eq!(s.orphans, 1);
        assert_eq!(s.removed, 0);
        assert_eq!(s.sites_unlinked, 1); // counted as "would unlink"
        assert!(sc_path.is_file(), "dry-run removed sidecar");
        assert!(target.is_file(), "dry-run removed link target");
    }

    #[test]
    fn missing_metadata_dir_is_noop() {
        let d = TempDir::new("nometa");
        let lib = d.0.join("lib");
        fs::create_dir_all(&lib).unwrap();
        let cfg = cfg_with(lib);
        let s = gc_with_known_detailed(&cfg, &BTreeSet::new(), false, true, None, None).summary;
        assert_eq!(s.scanned, 0);
        assert_eq!(s.errors, 0);
    }

    #[test]
    fn ignores_dotfiles_and_non_json() {
        let d = TempDir::new("filter");
        let lib = d.0.join("lib");
        fs::create_dir_all(lib.join(".metadata")).unwrap();
        // Lock-style dotfile and a stray .tmp.
        fs::write(lib.join(".metadata/.deadbeef.json.lock"), b"").unwrap();
        fs::write(lib.join(".metadata/notes.txt"), b"hi").unwrap();
        let cfg = cfg_with(lib);
        let s = gc_with_known_detailed(&cfg, &BTreeSet::new(), false, true, None, None).summary;
        assert_eq!(s.scanned, 0);
        assert_eq!(s.orphans, 0);
        assert_eq!(s.errors, 0);
    }

    fn seed_sidecar_with_category(
        lib: &std::path::Path,
        hash: &str,
        category: &str,
        rel: &str,
    ) -> PathBuf {
        let cat_root = lib.join(category);
        let site_dir = cat_root.join(rel);
        fs::create_dir_all(&site_dir).unwrap();
        let target = site_dir.join("Book");
        fs::write(&target, b"epub").unwrap();
        let sc = Sidecar {
            schema_version: SCHEMA_VERSION,
            info_hash_v1: hash.into(),
            info_hash_v2: None,
            name: "Book".into(),
            category: category.into(),
            content_path: format!("{}/Book", lib.display()),
            is_directory: false,
            size_bytes: 4,
            link_sites: vec![LinkSite {
                relative_path: rel.into(),
                resolved_path: target.to_string_lossy().into_owned(),
                created_at: "2026-05-11T00:00:00Z".into(),
                origin: Origin::PostProcess,
            }],
            last_applied_tags: vec![format!("link:{rel}")],
            last_applied_at: Some("2026-05-11T00:00:00Z".into()),
            warnings: vec![],
        };
        let p = sidecar::sidecar_path(lib, hash);
        sidecar::write(&p, &sc).unwrap();
        target
    }

    #[test]
    fn hash_filter_restricts_scan_case_insensitively() {
        let d = TempDir::new("hashfilter");
        let lib = d.0.join("lib");
        fs::create_dir_all(&lib).unwrap();
        let t_a = seed_sidecar_with_site(&lib, "aaaa1111", "Cat/A");
        let t_b = seed_sidecar_with_site(&lib, "bbbb2222", "Cat/B");
        let sc_a = sidecar::sidecar_path(&lib, "aaaa1111");
        let sc_b = sidecar::sidecar_path(&lib, "bbbb2222");

        let cfg = cfg_with(lib.clone());
        let s =
            gc_with_known_detailed(&cfg, &BTreeSet::new(), false, true, Some("AAAA1111"), None)
                .summary;

        assert_eq!(s.scanned, 1);
        assert_eq!(s.orphans, 1);
        assert_eq!(s.removed, 1);
        assert!(!sc_a.exists());
        assert!(!t_a.exists());
        assert!(sc_b.exists(), "untargeted sidecar removed");
        assert!(t_b.exists(), "untargeted link removed");
    }

    #[test]
    fn hash_filter_no_match_is_error() {
        let d = TempDir::new("hashnomatch");
        let lib = d.0.join("lib");
        fs::create_dir_all(&lib).unwrap();
        seed_sidecar_with_site(&lib, "aaaa1111", "Cat/A");

        let cfg = cfg_with(lib);
        let r = gc_with_known_detailed(&cfg, &BTreeSet::new(), false, true, Some("ffff"), None);
        assert_eq!(r.summary.errors, 1);
        assert_eq!(r.summary.scanned, 0);
    }

    #[test]
    fn category_filter_restricts_scan_case_insensitively() {
        let d = TempDir::new("catfilter");
        let lib = d.0.join("lib");
        fs::create_dir_all(&lib).unwrap();
        let t_a = seed_sidecar_with_category(&lib, "aaaa1111", "tracker.tld", "Cat/A");
        let t_b = seed_sidecar_with_category(&lib, "bbbb2222", "Other.Site", "Cat/B");
        let sc_a = sidecar::sidecar_path(&lib, "aaaa1111");
        let sc_b = sidecar::sidecar_path(&lib, "bbbb2222");

        let cfg = cfg_with(lib.clone());
        let s =
            gc_with_known_detailed(&cfg, &BTreeSet::new(), false, true, None, Some("TRACKER.TLD"))
                .summary;

        assert_eq!(s.scanned, 1);
        assert_eq!(s.orphans, 1);
        assert_eq!(s.removed, 1);
        assert!(!sc_a.exists());
        assert!(!t_a.exists());
        assert!(sc_b.exists());
        assert!(t_b.exists());
    }

    #[test]
    fn category_filter_no_match_is_error() {
        let d = TempDir::new("catnomatch");
        let lib = d.0.join("lib");
        fs::create_dir_all(&lib).unwrap();
        seed_sidecar_with_category(&lib, "aaaa1111", "tracker.tld", "Cat/A");

        let cfg = cfg_with(lib);
        let r =
            gc_with_known_detailed(&cfg, &BTreeSet::new(), false, true, None, Some("nope.tld"));
        assert_eq!(r.summary.errors, 1);
        assert_eq!(r.summary.scanned, 0);
    }

    // ---------- end-to-end against a qBittorrent mock ----------

    use crate::test_http::{ok_json, ok_text, spawn_mock};
    use std::path::Path;
    use std::sync::Arc;

    fn write_config_with_qb(root: &Path, lib: &Path, qb_url: &str, pw_env: &str) -> PathBuf {
        let trackers = root.join("trackers");
        fs::create_dir_all(&trackers).unwrap();
        let cfg = format!(
            r#"
[paths]
seed_root = "{lib}"
library_root = "{lib}"
trackers_root = "{trackers}"

[linking]
prefer = "hardlink"
windows_compat = false

[qbittorrent]
url = "{qb}"
username = "admin"
password_env = "{pw}"
"#,
            lib = lib.display(),
            trackers = trackers.display(),
            qb = qb_url,
            pw = pw_env,
        );
        let p = root.join("config.toml");
        fs::write(&p, cfg).unwrap();
        p
    }

    #[test]
    fn gc_end_to_end_against_qbittorrent_mock() {
        let d = TempDir::new("e2e");
        let lib = d.0.join("lib");
        fs::create_dir_all(&lib).unwrap();
        let target = seed_sidecar_with_site(&lib, "deadbeef", "Cat/Sub");
        let sc_path = sidecar::sidecar_path(&lib, "deadbeef");
        assert!(sc_path.is_file());
        assert!(target.is_file());

        let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log_h = log.clone();
        let (qb_url, _stop, _h) = spawn_mock(move |req| {
            let first = req.lines().next().unwrap_or("").to_string();
            log_h.lock().unwrap().push(first);
            if req.starts_with("POST /api/v2/auth/login ") {
                return ok_text("Ok.", "Set-Cookie: SID=tok; Path=/; HttpOnly\r\n");
            }
            if req.starts_with("GET /api/v2/torrents/info") {
                // Empty live set → seeded hash is an orphan.
                return ok_json("[]");
            }
            let body = "not found";
            format!(
                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        });

        let pw_env = format!("TQL_TEST_GC_PW_{}", std::process::id());
        std::env::set_var(&pw_env, "hunter2");
        let cfg_path = write_config_with_qb(&d.0, &lib, &qb_url, &pw_env);

        let res = do_run(&Args {
            dry_run: false,
            json: false,
            hash: None,
            category: None,
            config: Some(cfg_path),
        });
        std::env::remove_var(&pw_env);
        let s = res.expect("do_run failed").summary;

        assert_eq!(s.scanned, 1);
        assert_eq!(s.orphans, 1);
        assert_eq!(s.removed, 1);
        assert_eq!(s.sites_unlinked, 1);
        assert_eq!(s.errors, 0);
        assert!(!sc_path.exists(), "sidecar still present");
        assert!(!target.exists(), "link target still present");
        assert!(
            !lib.join("tracker.tld/Cat").exists(),
            "category subtree not pruned"
        );

        let lines = log.lock().unwrap().clone();
        let logins = lines
            .iter()
            .filter(|l| l.starts_with("POST /api/v2/auth/login"))
            .count();
        let infos = lines
            .iter()
            .filter(|l| l.starts_with("GET /api/v2/torrents/info"))
            .count();
        assert_eq!(logins, 1, "expected one login, got {lines:?}");
        assert_eq!(infos, 1, "expected one torrents/info, got {lines:?}");
        let login_idx = lines
            .iter()
            .position(|l| l.starts_with("POST /api/v2/auth/login"))
            .unwrap();
        let info_idx = lines
            .iter()
            .position(|l| l.starts_with("GET /api/v2/torrents/info"))
            .unwrap();
        assert!(login_idx < info_idx, "login must precede info: {lines:?}");
    }

    #[test]
    fn detailed_report_lists_kept_and_orphan_entries() {
        let d = TempDir::new("detailed");
        let lib = d.0.join("lib");
        fs::create_dir_all(&lib).unwrap();
        seed_sidecar_with_site(&lib, "aaaa", "Cat/Sub");
        seed_sidecar_with_site(&lib, "bbbb", "Cat/Sub2");

        let cfg = cfg_with(lib.clone());
        let mut known = BTreeSet::new();
        known.insert("aaaa".into());
        let r = gc_with_known_detailed(&cfg, &known, false, true, None, None);

        assert_eq!(r.summary.scanned, 2);
        assert_eq!(r.summary.kept, 1);
        assert_eq!(r.summary.orphans, 1);
        assert_eq!(r.summary.removed, 1);
        assert_eq!(r.summary.sites_unlinked, 1);
        assert_eq!(r.entries.len(), 2);

        // Sorted by hash.
        assert_eq!(r.entries[0].info_hash_v1, "aaaa");
        assert_eq!(r.entries[0].status, EntryStatus::Kept);
        assert!(!r.entries[0].removed);

        assert_eq!(r.entries[1].info_hash_v1, "bbbb");
        assert_eq!(r.entries[1].status, EntryStatus::Orphan);
        assert!(r.entries[1].removed);
        assert_eq!(r.entries[1].sites_unlinked, 1);
        assert!(r.entries[1].errors.is_empty());
    }

    #[test]
    fn render_json_shape_and_summary() {
        let report = Report {
            summary: Summary {
                scanned: 2,
                kept: 1,
                orphans: 1,
                removed: 1,
                sites_unlinked: 1,
                errors: 0,
            },
            entries: vec![
                Entry {
                    info_hash_v1: "aaaa".into(),
                    status: EntryStatus::Kept,
                    removed: false,
                    sites_unlinked: 0,
                    errors: vec![],
                },
                Entry {
                    info_hash_v1: "bbbb".into(),
                    status: EntryStatus::Orphan,
                    removed: true,
                    sites_unlinked: 1,
                    errors: vec![],
                },
            ],
        };
        let text = render_json(&report, false);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["dry_run"], false);
        assert_eq!(v["summary"]["scanned"], 2);
        assert_eq!(v["summary"]["kept"], 1);
        assert_eq!(v["summary"]["orphans"], 1);
        assert_eq!(v["summary"]["removed"], 1);
        assert_eq!(v["summary"]["sites_unlinked"], 1);
        assert_eq!(v["summary"]["errors"], 0);
        let entries = v["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["info_hash_v1"], "aaaa");
        assert_eq!(entries[0]["status"], "kept");
        assert_eq!(entries[0]["removed"], false);
        assert_eq!(entries[1]["status"], "orphan");
        assert_eq!(entries[1]["removed"], true);
        assert_eq!(entries[1]["sites_unlinked"], 1);
    }

    #[test]
    fn case_insensitive_hash_match() {
        let d = TempDir::new("case");
        let lib = d.0.join("lib");
        fs::create_dir_all(&lib).unwrap();
        seed_sidecar_with_site(&lib, "DEADBEEF", "Cat/Sub");
        let sc_path = sidecar::sidecar_path(&lib, "DEADBEEF");

        let cfg = cfg_with(lib);
        let mut known = BTreeSet::new();
        known.insert("deadbeef".into());
        let s = gc_with_known_detailed(&cfg, &known, false, true, None, None).summary;

        assert_eq!(s.kept, 1);
        assert_eq!(s.orphans, 0);
        assert!(sc_path.is_file());
    }
}
