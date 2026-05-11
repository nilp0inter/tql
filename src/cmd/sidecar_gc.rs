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
    /// Optional config-file override.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Summary {
    pub scanned: usize,
    pub kept: usize,
    pub orphans: usize,
    pub removed: usize,
    pub sites_unlinked: usize,
    pub errors: usize,
}

impl Summary {
    fn print(&self) {
        eprintln!(
            "tql sidecar gc: scanned {}, kept {}, orphans {}, removed {}, sites unlinked {}, errors {}",
            self.scanned, self.kept, self.orphans, self.removed, self.sites_unlinked, self.errors
        );
    }
}

pub fn run(args: Args) -> Result<(), u8> {
    match do_run(&args) {
        Ok(s) => {
            s.print();
            if s.errors > 0 {
                Err(1)
            } else {
                Ok(())
            }
        }
        Err(msg) => {
            eprintln!("tql sidecar gc: {msg}");
            Err(1)
        }
    }
}

fn do_run(args: &Args) -> Result<Summary, String> {
    let (_, cfg) = config::load(args.config.as_deref()).map_err(|e| format!("config: {e}"))?;
    let known = fetch_known_hashes(&cfg)?;
    Ok(gc_with_known(&cfg, &known, args.dry_run))
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

fn gc_with_known(
    cfg: &Config,
    known: &std::collections::BTreeSet<String>,
    dry_run: bool,
) -> Summary {
    let mut summary = Summary::default();
    let meta_dir = cfg.paths.library_root.join(".metadata");
    let entries = match fs::read_dir(&meta_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return summary,
        Err(e) => {
            eprintln!("tql sidecar gc: read {}: {e}", meta_dir.display());
            summary.errors += 1;
            return summary;
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

    for (hash, path) in sidecars {
        summary.scanned += 1;
        if known.contains(&hash) {
            summary.kept += 1;
            continue;
        }
        summary.orphans += 1;

        let sc = match sidecar::read(&path) {
            Ok(Some(sc)) => sc,
            Ok(None) => continue,
            Err(e) => {
                eprintln!("tql sidecar gc: read {}: {e}", path.display());
                summary.errors += 1;
                continue;
            }
        };

        let cat_root = cfg.paths.library_root.join(&sc.category);
        let mut had_err = false;
        for site in &sc.link_sites {
            let target = PathBuf::from(&site.resolved_path);
            if dry_run {
                println!("would unlink {}", target.display());
                summary.sites_unlinked += 1;
                continue;
            }
            match linking::unlink_site(&target, &cat_root) {
                Ok(()) => summary.sites_unlinked += 1,
                Err(e) => {
                    eprintln!("tql sidecar gc: unlink {}: {e}", target.display());
                    summary.errors += 1;
                    had_err = true;
                }
            }
        }

        if dry_run {
            println!("would remove sidecar {}", path.display());
            continue;
        }
        if had_err {
            // Leave the sidecar in place so a re-run can retry the remaining
            // sites; deleting it would forget the resolved paths.
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => summary.removed += 1,
            Err(e) => {
                eprintln!("tql sidecar gc: remove {}: {e}", path.display());
                summary.errors += 1;
                continue;
            }
        }
        // Best-effort: also drop the adjacent lock file.
        let lock = path.with_file_name(format!(
            ".{}.lock",
            path.file_name().unwrap().to_string_lossy()
        ));
        let _ = fs::remove_file(&lock);
    }

    summary
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
        let s = gc_with_known(&cfg, &known, false);

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
        let s = gc_with_known(&cfg, &known, false);

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
        let s = gc_with_known(&cfg, &known, true);

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
        let s = gc_with_known(&cfg, &BTreeSet::new(), false);
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
        let s = gc_with_known(&cfg, &BTreeSet::new(), false);
        assert_eq!(s.scanned, 0);
        assert_eq!(s.orphans, 0);
        assert_eq!(s.errors, 0);
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
        let s = gc_with_known(&cfg, &known, false);

        assert_eq!(s.kept, 1);
        assert_eq!(s.orphans, 0);
        assert!(sc_path.is_file());
    }
}
