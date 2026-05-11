//! `tql sidecar repair` — re-apply link sites that `tql sidecar verify` would
//! flag as `missing_resolved` or `inode_mismatch`.
//!
//! The repair direction is always *content_path → resolved_path*: we trust the
//! seed under `paths.seed_root` (recorded in the sidecar) and rebuild the
//! library-side link from it. Sidecars whose `content_path` itself is missing
//! are skipped (we have no source of truth to relink from); ditto for
//! malformed sidecars (`read_error`).
//!
//! `--dry-run` prints the actions it *would* take without touching the
//! filesystem. `--json` swaps the human-readable lines for a structured
//! report. Exit 1 on any per-site failure (or unrepairable issue), 0
//! otherwise.

use std::path::PathBuf;

use clap::Parser;
use serde_json::{json, Value as Json};

use crate::cmd::sidecar_verify::{scan, Issue};
use crate::config::{self, Config, LinkPrefer};
use crate::linking::{link_to_site, unlink_site, LinkOpts, LinkStrategy};
use crate::sidecar::{self, Sidecar};

#[derive(Parser, Debug)]
pub struct Args {
    /// Print the planned actions without touching the filesystem.
    #[arg(long)]
    pub dry_run: bool,
    /// Emit a JSON report instead of the human-readable lines.
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
            eprintln!("sidecar repair: {e}");
            return Err(1);
        }
    };
    repair(&cfg, args.dry_run, args.json, &mut std::io::stdout())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActionKind {
    Relink,  // missing_resolved: target absent → create it
    Replace, // inode_mismatch: target present but wrong → unlink + recreate
    Skip,    // unrepairable (missing_content, read_error)
}

impl ActionKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Relink => "relink",
            Self::Replace => "replace",
            Self::Skip => "skip",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Repaired,
    Skipped { reason: String },
    Failed { error: String },
    Planned, // dry-run only
}

impl Outcome {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Repaired => "repaired",
            Self::Skipped { .. } => "skipped",
            Self::Failed { .. } => "failed",
            Self::Planned => "planned",
        }
    }
}

#[derive(Debug, Clone)]
struct Action {
    hash: String,
    site: String,
    target: String,
    kind: ActionKind,
    outcome: Outcome,
}

pub(crate) fn repair<W: std::io::Write>(
    cfg: &Config,
    dry_run: bool,
    json: bool,
    out: &mut W,
) -> Result<(), u8> {
    let entries = match scan(&cfg.paths.library_root) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sidecar repair: {e}");
            return Err(1);
        }
    };

    let opts = LinkOpts {
        strategy: map_strategy(cfg.linking.prefer),
    };

    let mut actions: Vec<Action> = Vec::new();
    let scanned = entries.len();
    let mut ok_count = 0usize;

    for entry in &entries {
        if entry.issues.is_empty() {
            ok_count += 1;
            continue;
        }
        // Re-load the sidecar so we have content_path/category/is_directory.
        let sc_path = sidecar::sidecar_path(&cfg.paths.library_root, &entry.info_hash_v1);
        let sc = match sidecar::read(&sc_path) {
            Ok(Some(sc)) => sc,
            Ok(None) | Err(_) => {
                // Treat as one un-actionable skip per sidecar.
                actions.push(Action {
                    hash: entry.info_hash_v1.clone(),
                    site: String::new(),
                    target: String::new(),
                    kind: ActionKind::Skip,
                    outcome: Outcome::Skipped {
                        reason: "unreadable sidecar".into(),
                    },
                });
                continue;
            }
        };
        for issue in &entry.issues {
            actions.push(plan_then_apply(cfg, &sc, issue, &opts, dry_run));
        }
    }

    let repaired = actions
        .iter()
        .filter(|a| matches!(a.outcome, Outcome::Repaired))
        .count();
    let planned = actions
        .iter()
        .filter(|a| matches!(a.outcome, Outcome::Planned))
        .count();
    let skipped = actions
        .iter()
        .filter(|a| matches!(a.outcome, Outcome::Skipped { .. }))
        .count();
    let failed = actions
        .iter()
        .filter(|a| matches!(a.outcome, Outcome::Failed { .. }))
        .count();

    if json {
        let arr: Vec<Json> = actions
            .iter()
            .map(|a| {
                let mut o = json!({
                    "info_hash_v1": a.hash,
                    "site": a.site,
                    "target": a.target,
                    "action": a.kind.as_str(),
                    "outcome": a.outcome.as_str(),
                });
                if let Outcome::Failed { error } = &a.outcome {
                    o["error"] = json!(error);
                }
                if let Outcome::Skipped { reason } = &a.outcome {
                    o["reason"] = json!(reason);
                }
                o
            })
            .collect();
        let doc = json!({
            "dry_run": dry_run,
            "actions": arr,
            "summary": {
                "scanned": scanned,
                "ok": ok_count,
                "repaired": repaired,
                "planned": planned,
                "skipped": skipped,
                "failed": failed,
            },
        });
        match serde_json::to_string_pretty(&doc) {
            Ok(s) => {
                let _ = writeln!(out, "{s}");
            }
            Err(e) => {
                eprintln!("sidecar repair: serialize: {e}");
                return Err(1);
            }
        }
    } else {
        for a in &actions {
            let prefix = if dry_run { "would " } else { "" };
            match &a.outcome {
                Outcome::Repaired => {
                    let _ = writeln!(
                        out,
                        "{}  {} {} {} -> {}",
                        a.hash,
                        a.outcome.as_str(),
                        a.kind.as_str(),
                        a.site,
                        a.target
                    );
                }
                Outcome::Planned => {
                    let _ = writeln!(
                        out,
                        "{}  {}{} {} -> {}",
                        a.hash,
                        prefix,
                        a.kind.as_str(),
                        a.site,
                        a.target
                    );
                }
                Outcome::Skipped { reason } => {
                    let _ = writeln!(out, "{}  skip {} {} ({})", a.hash, a.site, a.target, reason);
                }
                Outcome::Failed { error } => {
                    let _ = writeln!(
                        out,
                        "{}  fail {} {} -> {}: {}",
                        a.hash,
                        a.kind.as_str(),
                        a.site,
                        a.target,
                        error
                    );
                }
            }
        }
        let _ = writeln!(
            out,
            "scanned={scanned} ok={ok_count} repaired={repaired} planned={planned} \
             skipped={skipped} failed={failed}"
        );
    }

    if failed > 0 || skipped > 0 {
        Err(1)
    } else {
        Ok(())
    }
}

fn plan_then_apply(
    cfg: &Config,
    sc: &Sidecar,
    issue: &Issue,
    opts: &LinkOpts,
    dry_run: bool,
) -> Action {
    let hash = sc.info_hash_v1.clone();
    let stop_at = cfg.paths.library_root.join(&sc.category);

    let (kind, site, target) = match issue {
        Issue::MissingResolved { site, path } => (ActionKind::Relink, site.clone(), path.clone()),
        Issue::InodeMismatch { site, path } => (ActionKind::Replace, site.clone(), path.clone()),
        Issue::MissingContent { path } => {
            return Action {
                hash,
                site: String::new(),
                target: path.clone(),
                kind: ActionKind::Skip,
                outcome: Outcome::Skipped {
                    reason: "missing content_path".into(),
                },
            };
        }
        Issue::ReadError(msg) => {
            return Action {
                hash,
                site: String::new(),
                target: String::new(),
                kind: ActionKind::Skip,
                outcome: Outcome::Skipped {
                    reason: format!("read_error: {msg}"),
                },
            };
        }
    };

    if dry_run {
        return Action {
            hash,
            site,
            target,
            kind,
            outcome: Outcome::Planned,
        };
    }

    let target_path = std::path::PathBuf::from(&target);
    let source_path = std::path::PathBuf::from(&sc.content_path);

    if matches!(kind, ActionKind::Replace) {
        if let Err(e) = unlink_site(&target_path, &stop_at) {
            return Action {
                hash,
                site,
                target,
                kind,
                outcome: Outcome::Failed {
                    error: format!("unlink before replace: {e}"),
                },
            };
        }
    }

    match link_to_site(&source_path, &target_path, opts) {
        Ok(_) => Action {
            hash,
            site,
            target,
            kind,
            outcome: Outcome::Repaired,
        },
        Err(e) => Action {
            hash,
            site,
            target,
            kind,
            outcome: Outcome::Failed {
                error: e.to_string(),
            },
        },
    }
}

fn map_strategy(p: LinkPrefer) -> LinkStrategy {
    match p {
        LinkPrefer::Hardlink => LinkStrategy::Hardlink,
        LinkPrefer::Reflink => LinkStrategy::Reflink,
        LinkPrefer::ReflinkOrHardlink => LinkStrategy::ReflinkOrHardlink,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Api, Linking, Mcp, Media, Notify, Paths, QBittorrent, Reconcile, Scripting,
    };
    use crate::sidecar::{LinkSite, Origin, Sidecar, SCHEMA_VERSION};
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    fn tmp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nano = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        p.push(format!("tql-repair-{tag}-{pid}-{nano}"));
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
    fn repair_recreates_missing_resolved_site_as_hardlink() {
        let lib = tmp_dir("missing");
        let seed = lib.join("seed.bin");
        std::fs::write(&seed, b"hello").unwrap();
        // Site doesn't exist on disk; sidecar still records it.
        let target = lib.join("demo.org").join("Cat").join("Book");

        let sc = mk_file_sidecar("aaa", &seed, vec![("Cat", &target)]);
        write_sidecar(&lib, &sc);

        let cfg = cfg_with_lib(lib.clone());
        let mut buf: Vec<u8> = Vec::new();
        let r = repair(&cfg, false, false, &mut buf);
        assert!(r.is_ok(), "got: {}", String::from_utf8_lossy(&buf));

        // Target exists and shares the seed's inode.
        let seed_meta = std::fs::metadata(&seed).unwrap();
        let tgt_meta = std::fs::metadata(&target).unwrap();
        assert_eq!(seed_meta.dev(), tgt_meta.dev());
        assert_eq!(seed_meta.ino(), tgt_meta.ino());

        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("repaired relink"));
        assert!(s.contains("repaired=1"));
    }

    #[test]
    fn repair_replaces_inode_mismatched_site() {
        let lib = tmp_dir("mismatch");
        let seed = lib.join("seed.bin");
        std::fs::write(&seed, b"hello").unwrap();
        let site_dir = lib.join("demo.org").join("Cat");
        std::fs::create_dir_all(&site_dir).unwrap();
        let site = site_dir.join("Book");
        // Independent copy — different inode.
        std::fs::write(&site, b"hello").unwrap();
        let bad_ino = std::fs::metadata(&site).unwrap().ino();

        let sc = mk_file_sidecar("bbb", &seed, vec![("Cat", &site)]);
        write_sidecar(&lib, &sc);

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = repair(&cfg, false, false, &mut buf);
        assert!(r.is_ok(), "got: {}", String::from_utf8_lossy(&buf));

        let seed_ino = std::fs::metadata(&seed).unwrap().ino();
        let new_ino = std::fs::metadata(&site).unwrap().ino();
        assert_eq!(seed_ino, new_ino);
        assert_ne!(bad_ino, new_ino);
    }

    #[test]
    fn repair_dry_run_does_not_touch_filesystem() {
        let lib = tmp_dir("dry");
        let seed = lib.join("seed.bin");
        std::fs::write(&seed, b"hello").unwrap();
        let target = lib.join("demo.org").join("Cat").join("Book");
        let sc = mk_file_sidecar("ccc", &seed, vec![("Cat", &target)]);
        write_sidecar(&lib, &sc);

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = repair(&cfg, true, false, &mut buf);
        // Planned actions don't count as failures, but the original verify
        // would have flagged the issue — exit code is 0 in dry-run when
        // nothing was skipped/failed.
        assert!(r.is_ok());
        assert!(!target.exists());
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("would relink"));
        assert!(s.contains("planned=1"));
    }

    #[test]
    fn repair_skips_when_content_is_missing() {
        let lib = tmp_dir("nocontent");
        let seed = lib.join("seed.bin"); // never created
        let target = lib.join("demo.org").join("Cat").join("Book");
        let sc = mk_file_sidecar("ddd", &seed, vec![("Cat", &target)]);
        write_sidecar(&lib, &sc);

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = repair(&cfg, false, false, &mut buf);
        assert_eq!(r, Err(1));
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("skip"));
        assert!(s.contains("missing content_path"));
    }

    #[test]
    fn repair_no_sidecars_is_ok_zero() {
        let lib = tmp_dir("empty");
        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = repair(&cfg, false, false, &mut buf);
        assert!(r.is_ok());
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("scanned=0"));
    }

    #[test]
    fn repair_clean_world_is_noop() {
        let lib = tmp_dir("clean");
        let seed = lib.join("seed.bin");
        std::fs::write(&seed, b"hello").unwrap();
        let site_dir = lib.join("demo.org").join("Cat");
        std::fs::create_dir_all(&site_dir).unwrap();
        let site = site_dir.join("Book");
        std::fs::hard_link(&seed, &site).unwrap();

        let sc = mk_file_sidecar("eee", &seed, vec![("Cat", &site)]);
        write_sidecar(&lib, &sc);

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = repair(&cfg, false, false, &mut buf);
        assert!(r.is_ok());
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("scanned=1 ok=1 repaired=0"));
    }

    #[test]
    fn repair_json_shape_includes_summary_and_actions() {
        let lib = tmp_dir("json");
        let seed = lib.join("seed.bin");
        std::fs::write(&seed, b"hello").unwrap();
        let target = lib.join("demo.org").join("Cat").join("Book");
        let sc = mk_file_sidecar("fff", &seed, vec![("Cat", &target)]);
        write_sidecar(&lib, &sc);

        let cfg = cfg_with_lib(lib);
        let mut buf: Vec<u8> = Vec::new();
        let r = repair(&cfg, false, true, &mut buf);
        assert!(r.is_ok());
        let parsed: Json = serde_json::from_slice(&buf).unwrap();
        assert_eq!(parsed["dry_run"], false);
        let actions = parsed["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["info_hash_v1"], "fff");
        assert_eq!(actions[0]["action"], "relink");
        assert_eq!(actions[0]["outcome"], "repaired");
        assert_eq!(parsed["summary"]["scanned"], 1);
        assert_eq!(parsed["summary"]["repaired"], 1);
        assert_eq!(parsed["summary"]["failed"], 0);
    }
}
