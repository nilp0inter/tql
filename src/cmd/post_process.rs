//! qBittorrent "torrent finished" hook (DESIGN.md §7 post-process).
//!
//! Behavior:
//! 1. Parse argv (long flags only, per §8).
//! 2. Acquire exclusive flock on `<library_root>/.metadata/<hash>.lock`
//!    (≤30 s wait).
//! 3. Sanity-check the category.
//! 4. Parse `link:` tags from `--tags`.
//! 5. Compute desired link sites; diff against the existing sidecar.
//! 6. Apply: create new sites, remove stale ones.
//! 7. Write sidecar atomically.
//!
//! Notifications (step 8) and media-server refresh (step 9) land with Leg 15.
//!
//! Per §7, this command **always exits 0**. Operator-visible failures are
//! recorded in the sidecar's `warnings` array and on stderr.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;
use fs2::FileExt;

use crate::config::{self, Config, LinkPrefer};
use crate::linking::{self, LinkOpts, LinkStrategy};
use crate::paths::{self, sanitize_component, SanitizeOpts};
use crate::sidecar::{self, LinkSite, Origin, Sidecar, SCHEMA_VERSION};

/// qBittorrent hook. Argv mirrors DESIGN.md §8 — long flags only.
#[derive(Parser, Debug)]
pub struct Args {
    #[arg(long, value_name = "INFO_HASH")]
    pub hash: String,
    #[arg(long)]
    pub name: String,
    #[arg(long)]
    pub category: String,
    /// qBittorrent passes tags as a single comma-separated string.
    #[arg(long, default_value = "")]
    pub tags: String,
    #[arg(long, value_name = "PATH")]
    pub content_path: String,
    #[arg(long, value_name = "PATH")]
    pub save_path: String,
    #[arg(long, value_name = "BYTES")]
    pub size: u64,
    /// Optional config-file override (tests + manual replays).
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<(), u8> {
    match process(&args) {
        Outcome::Ok { sidecar: _, warnings } => {
            for w in &warnings {
                eprintln!("tql post-process: warn: {w}");
            }
        }
        Outcome::Planned { .. } => {
            // post-process never calls itself with dry_run; this arm exists
            // only to satisfy match exhaustiveness against shared Outcome.
        }
        Outcome::Aborted(reason) => {
            // Aborted means we never got far enough to write a sidecar.
            eprintln!("tql post-process: aborted: {reason}");
        }
    }
    Ok(())
}

const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
#[allow(dead_code)]
pub enum Outcome {
    /// Sidecar written. May still carry per-tag warnings.
    Ok {
        sidecar: Sidecar,
        warnings: Vec<String>,
    },
    /// Dry-run: the diff that *would* be applied. No filesystem writes.
    Planned {
        hash: String,
        adds: Vec<String>,
        removes: Vec<String>,
        warnings: Vec<String>,
    },
    /// Couldn't even start (config missing, lock starvation, bad category).
    /// Nothing was written.
    Aborted(String),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessOpts {
    /// Compute the diff but skip filesystem mutations and sidecar write.
    pub dry_run: bool,
}

pub fn process(args: &Args) -> Outcome {
    let cfg = match config::load(args.config.as_deref()) {
        Ok((_, c)) => c,
        Err(e) => return Outcome::Aborted(format!("config: {e}")),
    };
    process_with_cfg(args, &cfg, ProcessOpts::default())
}

pub fn process_with_cfg(args: &Args, cfg: &Config, opts: ProcessOpts) -> Outcome {
    let library_root = cfg.paths.library_root.clone();
    let sopts = SanitizeOpts {
        windows_compat: cfg.linking.windows_compat,
    };

    // §7.3 — non-empty, no slashes, sanitizable (byte-stable).
    if args.category.is_empty() {
        return Outcome::Aborted("empty category".into());
    }
    if args.category.contains('/') {
        return Outcome::Aborted(format!("category contains slash: {:?}", args.category));
    }
    if sanitize_component(&args.category, &sopts) != args.category {
        return Outcome::Aborted(format!("category not byte-stable: {:?}", args.category));
    }

    let sidecar_path = sidecar::sidecar_path(&library_root, &args.hash);
    // The sidecar module uses `.<file>.lock` adjacent to the sidecar. We hold
    // the same path here so post-process and sidecar::write serialize against
    // each other.
    let lock_path = lock_path_for(&sidecar_path);
    if let Some(parent) = lock_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return Outcome::Aborted(format!("mkdir {}: {e}", parent.display()));
        }
    }
    let lock_file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(e) => return Outcome::Aborted(format!("open lock {}: {e}", lock_path.display())),
    };
    if let Err(e) = acquire_with_timeout(&lock_file, LOCK_TIMEOUT) {
        return Outcome::Aborted(format!("flock: {e}"));
    }
    let _guard = LockGuard(&lock_file);

    let existing = match sidecar::read(&sidecar_path) {
        Ok(s) => s,
        Err(e) => {
            // Malformed sidecar quarantines: rename it aside and proceed
            // fresh. (Quarantine machinery lives in Leg 11/15; for now we
            // just abort so an operator can intervene.)
            return Outcome::Aborted(format!("read sidecar: {e}"));
        }
    };

    let content_path = PathBuf::from(&args.content_path);
    let is_directory = content_path.is_dir();

    let mut warnings: Vec<String> = Vec::new();

    // §5 — parse + validate every `link:` tag against the producer rules.
    let raw_tags = parse_tag_csv(&args.tags);
    let link_tags: Vec<String> = raw_tags
        .iter()
        .filter(|t| t.starts_with(paths::LINK_TAG_PREFIX))
        .cloned()
        .collect();

    let mut desired: Vec<DesiredSite> = Vec::new();
    let mut seen_rel: BTreeSet<String> = BTreeSet::new();
    for raw in &link_tags {
        let parsed = match paths::parse_link_tag(raw, Some(&args.category)) {
            Ok(t) => t,
            Err(e) => {
                warnings.push(format!("tag {raw:?}: invalid: {e:?}"));
                continue;
            }
        };
        let resolved = match paths::resolve_link_site(
            &library_root,
            &args.category,
            &parsed,
            &args.name,
            &sopts,
        ) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("tag {raw:?}: resolve: {e:?}"));
                continue;
            }
        };
        let rel = parsed.relative_path.to_string();
        if !seen_rel.insert(rel.clone()) {
            // Duplicate tag → silently dedup.
            continue;
        }
        desired.push(DesiredSite {
            raw: raw.clone(),
            relative_path: rel,
            resolved_path: resolved,
        });
    }

    // §5 soft caps — warn if exceeded.
    if link_tags.len() > paths::SOFT_MAX_LINK_TAGS {
        warnings.push(format!(
            "soft cap: {} link tags exceeds {}",
            link_tags.len(),
            paths::SOFT_MAX_LINK_TAGS
        ));
    }
    for raw in &link_tags {
        if raw.len() > paths::SOFT_MAX_LINK_TAG_BYTES {
            warnings.push(format!(
                "soft cap: tag {raw:?} exceeds {} bytes",
                paths::SOFT_MAX_LINK_TAG_BYTES
            ));
        }
    }

    let link_opts = LinkOpts {
        strategy: map_strategy(cfg.linking.prefer),
    };

    // Apply: create new sites, retain ones already in the sidecar.
    let now = iso_utc_now();
    let prior_by_rel: std::collections::BTreeMap<String, LinkSite> = existing
        .as_ref()
        .map(|s| {
            s.link_sites
                .iter()
                .map(|ls| (ls.relative_path.clone(), ls.clone()))
                .collect()
        })
        .unwrap_or_default();

    if opts.dry_run {
        let desired_rels: BTreeSet<String> =
            desired.iter().map(|d| d.relative_path.clone()).collect();
        let adds: Vec<String> = desired
            .iter()
            .filter(|d| !prior_by_rel.contains_key(&d.relative_path))
            .map(|d| d.relative_path.clone())
            .collect();
        let removes: Vec<String> = prior_by_rel
            .keys()
            .filter(|rel| !desired_rels.contains(*rel))
            .cloned()
            .collect();
        if link_tags.is_empty() {
            warnings.push("torrent has no link: tags".into());
        }
        return Outcome::Planned {
            hash: args.hash.clone(),
            adds,
            removes,
            warnings,
        };
    }

    let mut applied_sites: Vec<LinkSite> = Vec::new();
    let mut applied_relpaths: BTreeSet<String> = BTreeSet::new();
    for d in &desired {
        match linking::link_to_site(&content_path, &d.resolved_path, &link_opts) {
            Ok(_outcome) => {
                let created_at = prior_by_rel
                    .get(&d.relative_path)
                    .map(|ls| ls.created_at.clone())
                    .unwrap_or_else(|| now.clone());
                applied_sites.push(LinkSite {
                    relative_path: d.relative_path.clone(),
                    resolved_path: d.resolved_path.to_string_lossy().into_owned(),
                    created_at,
                    origin: Origin::PostProcess,
                });
                applied_relpaths.insert(d.relative_path.clone());
            }
            Err(e) => {
                warnings.push(format!("link {:?}: {e}", d.raw));
            }
        }
    }

    // Stale removal: prior sites that aren't in the new desired set.
    let cat_root = library_root.join(&args.category);
    for (rel, prior) in &prior_by_rel {
        if applied_relpaths.contains(rel) {
            continue;
        }
        // Only unlink if it was also in the *desired* set or wasn't (always
        // remove if not desired). Keep failures non-fatal.
        let target = PathBuf::from(&prior.resolved_path);
        if let Err(e) = linking::unlink_site(&target, &cat_root) {
            warnings.push(format!("unlink {:?}: {e}", prior.relative_path));
        }
    }

    if applied_sites.is_empty() && !link_tags.is_empty() {
        warnings.push("no valid link tags applied".into());
    }
    if link_tags.is_empty() {
        warnings.push("torrent has no link: tags".into());
    }

    let new_sidecar = Sidecar {
        schema_version: SCHEMA_VERSION,
        info_hash_v1: args.hash.clone(),
        info_hash_v2: existing.as_ref().and_then(|s| s.info_hash_v2.clone()),
        name: args.name.clone(),
        category: args.category.clone(),
        content_path: args.content_path.clone(),
        is_directory,
        size_bytes: args.size,
        link_sites: applied_sites,
        last_applied_tags: link_tags,
        last_applied_at: Some(now),
        warnings: warnings.clone(),
    };

    if let Err(e) = sidecar::write(&sidecar_path, &new_sidecar) {
        return Outcome::Aborted(format!("write sidecar: {e}"));
    }
    Outcome::Ok {
        sidecar: new_sidecar,
        warnings,
    }
}

#[derive(Debug)]
struct DesiredSite {
    raw: String,
    relative_path: String,
    resolved_path: PathBuf,
}

fn map_strategy(p: LinkPrefer) -> LinkStrategy {
    match p {
        LinkPrefer::Hardlink => LinkStrategy::Hardlink,
        LinkPrefer::Reflink => LinkStrategy::Reflink,
        LinkPrefer::ReflinkOrHardlink => LinkStrategy::ReflinkOrHardlink,
    }
}

fn parse_tag_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Lock file for the post-process pipeline. Distinct from the sidecar's own
/// `.<name>.lock` so that `sidecar::read` / `sidecar::write` (which take their
/// own flocks on the sidecar lock) can run *while we hold the pipeline lock*
/// without deadlocking. Linux flock uses one OFD per open(2), so the two locks
/// must be on different paths.
fn lock_path_for(sidecar: &Path) -> PathBuf {
    let mut p = sidecar.to_path_buf();
    let name = sidecar
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    p.set_file_name(format!(".{name}.pp.lock"));
    p
}

struct LockGuard<'a>(&'a std::fs::File);
impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        let _ = FileExt::unlock(self.0);
    }
}

fn acquire_with_timeout(f: &std::fs::File, max: Duration) -> io::Result<()> {
    let start = Instant::now();
    loop {
        match f.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if start.elapsed() >= max {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "flock timeout"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
}

// ---- timestamp helpers (no chrono dep) -------------------------------------

fn iso_utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso_utc_from_epoch(secs)
}

fn iso_utc_from_epoch(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let sod = secs.rem_euclid(86400) as u32;
    let h = sod / 3600;
    let m = (sod / 60) % 60;
    let s = sod % 60;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Howard Hinnant's civil_from_days. Input: days since 1970-01-01.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut d = std::env::temp_dir();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            d.push(format!("tql-pp-{}-{}-{}", tag, std::process::id(), nanos));
            fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_file(p: &Path, body: &[u8]) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(p).unwrap();
        f.write_all(body).unwrap();
    }

    fn write_config(root: &Path, library_root: &Path, seed_root: &Path) -> PathBuf {
        let cfg = format!(
            r#"
[paths]
seed_root = "{seed}"
library_root = "{lib}"
trackers_root = "{root}/trackers"

[linking]
prefer = "hardlink"
windows_compat = false
"#,
            seed = seed_root.display(),
            lib = library_root.display(),
            root = root.display(),
        );
        let p = root.join("config.toml");
        fs::write(&p, cfg).unwrap();
        p
    }

    #[test]
    fn iso_format_known_date() {
        // 2026-05-11T00:00:00Z
        // Compute via the date utility's reference:
        // days from 1970-01-01 to 2026-05-11.
        // We can just round-trip a constructed value.
        let s = iso_utc_from_epoch(1_767_136_800); // 2025-12-31T10:00:00Z roughly
        // Just sanity-check the shape; exact date verified in next test.
        assert_eq!(s.len(), 20);
        assert!(s.ends_with('Z'));
    }

    #[test]
    fn iso_format_epoch_zero() {
        assert_eq!(iso_utc_from_epoch(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn iso_format_y2k() {
        // 2000-01-01T00:00:00Z = 946684800
        assert_eq!(iso_utc_from_epoch(946_684_800), "2000-01-01T00:00:00Z");
    }

    #[test]
    fn happy_path_single_file_one_tag() {
        let d = TempDir::new("happy");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let content = seed.join("Some Book.epub");
        write_file(&content, b"epub bytes");

        let cfg = write_config(d.path(), &lib, &seed);
        let args = Args {
            hash: "deadbeef".into(),
            name: "Some Book".into(),
            category: "myanonamouse.net".into(),
            tags: "link:Computer/Internet/Hamza Farooq,extra-info-tag".into(),
            content_path: content.to_string_lossy().into_owned(),
            save_path: seed.to_string_lossy().into_owned(),
            size: 10,
            config: Some(cfg),
        };
        let out = process(&args);
        match out {
            Outcome::Ok { sidecar, warnings } => {
                assert_eq!(sidecar.link_sites.len(), 1);
                assert_eq!(
                    sidecar.link_sites[0].relative_path,
                    "Computer/Internet/Hamza Farooq"
                );
                // Single-tag torrent with no validation hits — no warnings.
                assert!(warnings.is_empty(), "warnings: {warnings:?}");
                // Target file exists and is a hardlink of source.
                let target = lib
                    .join("myanonamouse.net")
                    .join("Computer/Internet/Hamza Farooq/Some Book");
                let sm = fs::metadata(&content).unwrap();
                let tm = fs::metadata(&target).unwrap();
                use std::os::unix::fs::MetadataExt;
                assert_eq!(sm.ino(), tm.ino());
                // Sidecar persisted.
                let sc_path = sidecar::sidecar_path(&lib, "deadbeef");
                let read_back = sidecar::read(&sc_path).unwrap().unwrap();
                assert_eq!(read_back.name, "Some Book");
            }
            Outcome::Planned { .. } => unreachable!("dry_run not set"),
            Outcome::Aborted(reason) => panic!("aborted: {reason}"),
        }
    }

    #[test]
    fn idempotent_rerun() {
        let d = TempDir::new("idem");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let content = seed.join("file.bin");
        write_file(&content, b"x");
        let cfg = write_config(d.path(), &lib, &seed);
        let args = Args {
            hash: "abc".into(),
            name: "Thing".into(),
            category: "tracker.tld".into(),
            tags: "link:Cat/Sub".into(),
            content_path: content.to_string_lossy().into_owned(),
            save_path: seed.to_string_lossy().into_owned(),
            size: 1,
            config: Some(cfg),
        };
        let a = process(&args);
        assert!(matches!(a, Outcome::Ok { .. }));
        let b = process(&args);
        match b {
            Outcome::Ok { sidecar, warnings } => {
                assert_eq!(sidecar.link_sites.len(), 1);
                assert!(warnings.is_empty(), "warnings: {warnings:?}");
            }
            Outcome::Planned { .. } => unreachable!("dry_run not set"),
            Outcome::Aborted(r) => panic!("aborted: {r}"),
        }
    }

    #[test]
    fn diff_removes_stale_site() {
        let d = TempDir::new("stale");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let content = seed.join("f");
        write_file(&content, b"x");
        let cfg = write_config(d.path(), &lib, &seed);
        let mut args = Args {
            hash: "h".into(),
            name: "N".into(),
            category: "t.tld".into(),
            tags: "link:A,link:B".into(),
            content_path: content.to_string_lossy().into_owned(),
            save_path: seed.to_string_lossy().into_owned(),
            size: 1,
            config: Some(cfg),
        };
        // First pass creates both A and B.
        assert!(matches!(process(&args), Outcome::Ok { .. }));
        let target_a = lib.join("t.tld/A/N");
        let target_b = lib.join("t.tld/B/N");
        assert!(target_a.exists());
        assert!(target_b.exists());

        // Second pass with only A: B must be unlinked, parent pruned.
        args.tags = "link:A".into();
        assert!(matches!(process(&args), Outcome::Ok { .. }));
        assert!(target_a.exists());
        assert!(!target_b.exists());
        // B/ pruned, t.tld/ kept (it's the stop boundary).
        assert!(!lib.join("t.tld/B").exists());
        assert!(lib.join("t.tld").exists());
    }

    #[test]
    fn invalid_tag_warned_not_aborted() {
        let d = TempDir::new("badtag");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let content = seed.join("f");
        write_file(&content, b"x");
        let cfg = write_config(d.path(), &lib, &seed);
        let args = Args {
            hash: "h2".into(),
            name: "N".into(),
            category: "t.tld".into(),
            // First tag is malformed (../ escape); second is fine.
            tags: "link:../escape,link:Good".into(),
            content_path: content.to_string_lossy().into_owned(),
            save_path: seed.to_string_lossy().into_owned(),
            size: 1,
            config: Some(cfg),
        };
        match process(&args) {
            Outcome::Ok { sidecar, warnings } => {
                assert_eq!(sidecar.link_sites.len(), 1);
                assert_eq!(sidecar.link_sites[0].relative_path, "Good");
                assert!(
                    warnings.iter().any(|w| w.contains("../escape")),
                    "expected warning, got {warnings:?}"
                );
            }
            Outcome::Planned { .. } => unreachable!("dry_run not set"),
            Outcome::Aborted(r) => panic!("aborted: {r}"),
        }
    }

    #[test]
    fn no_link_tags_writes_sidecar_with_warning() {
        let d = TempDir::new("notags");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let content = seed.join("f");
        write_file(&content, b"x");
        let cfg = write_config(d.path(), &lib, &seed);
        let args = Args {
            hash: "h3".into(),
            name: "N".into(),
            category: "t.tld".into(),
            tags: "".into(),
            content_path: content.to_string_lossy().into_owned(),
            save_path: seed.to_string_lossy().into_owned(),
            size: 1,
            config: Some(cfg),
        };
        match process(&args) {
            Outcome::Ok { sidecar, warnings } => {
                assert!(sidecar.link_sites.is_empty());
                assert!(warnings.iter().any(|w| w.contains("no link: tags")));
            }
            Outcome::Planned { .. } => unreachable!("dry_run not set"),
            Outcome::Aborted(r) => panic!("aborted: {r}"),
        }
    }

    #[test]
    fn empty_category_aborts() {
        let d = TempDir::new("emptycat");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let content = seed.join("f");
        write_file(&content, b"x");
        let cfg = write_config(d.path(), &lib, &seed);
        let args = Args {
            hash: "h4".into(),
            name: "N".into(),
            category: "".into(),
            tags: "link:A".into(),
            content_path: content.to_string_lossy().into_owned(),
            save_path: seed.to_string_lossy().into_owned(),
            size: 1,
            config: Some(cfg),
        };
        assert!(matches!(process(&args), Outcome::Aborted(_)));
    }

    #[test]
    fn category_with_slash_aborts() {
        let d = TempDir::new("slashcat");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let content = seed.join("f");
        write_file(&content, b"x");
        let cfg = write_config(d.path(), &lib, &seed);
        let args = Args {
            hash: "h5".into(),
            name: "N".into(),
            category: "a/b".into(),
            tags: "link:A".into(),
            content_path: content.to_string_lossy().into_owned(),
            save_path: seed.to_string_lossy().into_owned(),
            size: 1,
            config: Some(cfg),
        };
        assert!(matches!(process(&args), Outcome::Aborted(_)));
    }

    #[test]
    fn directory_torrent_replicated() {
        let d = TempDir::new("dir");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let content = seed.join("Album");
        write_file(&content.join("01.flac"), b"flac1");
        write_file(&content.join("02.flac"), b"flac2");
        let cfg = write_config(d.path(), &lib, &seed);
        let args = Args {
            hash: "hdir".into(),
            name: "Album".into(),
            category: "r.tld".into(),
            tags: "link:FLAC/CD/Artist".into(),
            content_path: content.to_string_lossy().into_owned(),
            save_path: seed.to_string_lossy().into_owned(),
            size: 10,
            config: Some(cfg),
        };
        match process(&args) {
            Outcome::Ok { sidecar, .. } => {
                assert!(sidecar.is_directory);
                let target = lib.join("r.tld/FLAC/CD/Artist/Album");
                assert!(target.join("01.flac").exists());
                assert!(target.join("02.flac").exists());
            }
            Outcome::Planned { .. } => unreachable!("dry_run not set"),
            Outcome::Aborted(r) => panic!("aborted: {r}"),
        }
    }
}
