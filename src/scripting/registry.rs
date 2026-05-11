//! Tracker registry: discover and load every tracker under `<trackers_root>`.
//!
//! Each tracker is a directory containing at least:
//!
//! * `manifest.toml` — parsed via [`crate::scripting::manifest`]
//! * `classify.rhai` — compiled to a Rhai `AST` against a sandboxed engine
//!
//! `fixtures/` is *not* loaded here; the `tql test` runner (Leg 9b) discovers
//! them on demand.
//!
//! Loading is best-effort: a single broken tracker doesn't break the rest.
//! Errors are collected per-tracker so callers (`tql doctor`, `tql test`) can
//! report all problems at once.
//!
//! Trackers are keyed by **manifest `name`**, not by directory name. Two
//! manifests with the same `name` are a load error (the second is rejected).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rhai::{Engine, AST};

use crate::scripting::host;
use crate::scripting::manifest::{self, Manifest, ManifestError};
use crate::scripting::types::ClassifyError;

/// One loaded tracker: parsed manifest + compiled script + on-disk paths.
///
/// `Arc<AST>` so `Tracker` can be cheaply cloned into per-request task scopes
/// once the transports layer (Legs 13/14) wires this into tokio.
#[derive(Debug, Clone)]
pub struct Tracker {
    pub manifest: Manifest,
    pub script: Arc<AST>,
    /// Tracker directory, e.g. `<trackers_root>/myanonamouse/`.
    pub dir: PathBuf,
}

/// Why a single tracker failed to load.
#[derive(Debug)]
pub enum TrackerLoadError {
    /// The directory had no `manifest.toml`.
    MissingManifest,
    /// The directory had no `classify.rhai`.
    MissingScript,
    /// Filesystem error reading the directory or its files.
    Io(std::io::Error),
    /// Manifest parse / validation failure.
    Manifest(ManifestError),
    /// Rhai compile error.
    Compile(ClassifyError),
    /// Two trackers in the registry resolved to the same `name`. The first
    /// wins; this carries the *rejected* directory.
    DuplicateName { existing_dir: PathBuf },
}

impl std::fmt::Display for TrackerLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrackerLoadError::MissingManifest => write!(f, "missing manifest.toml"),
            TrackerLoadError::MissingScript => write!(f, "missing classify.rhai"),
            TrackerLoadError::Io(e) => write!(f, "io error: {e}"),
            TrackerLoadError::Manifest(e) => write!(f, "{e}"),
            TrackerLoadError::Compile(e) => write!(f, "{e}"),
            TrackerLoadError::DuplicateName { existing_dir } => {
                write!(
                    f,
                    "duplicate tracker name (already loaded from {})",
                    existing_dir.display()
                )
            }
        }
    }
}

impl std::error::Error for TrackerLoadError {}

/// One per-tracker load failure as reported by [`load_dir`]. The `dir` is the
/// offending tracker directory; `error` is what went wrong.
#[derive(Debug)]
pub struct LoadFailure {
    pub dir: PathBuf,
    pub error: TrackerLoadError,
}

impl std::fmt::Display for LoadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.dir.display(), self.error)
    }
}

/// Loaded registry of trackers, keyed by manifest `name`.
///
/// `BTreeMap` (not `HashMap`) so iteration order is stable — used by
/// `tql cli` (no args) and `GET /trackers` to render a deterministic list.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    trackers: BTreeMap<String, Tracker>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, name: &str) -> Option<&Tracker> {
        self.trackers.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.trackers.keys().map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Tracker)> {
        self.trackers.iter().map(|(n, t)| (n.as_str(), t))
    }

    pub fn len(&self) -> usize {
        self.trackers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.trackers.is_empty()
    }
}

/// Cheaply-cloneable, hot-swappable registry handle. Used by long-running
/// servers (`tql api`, `tql mcp --http`) so a SIGHUP-driven reload can swap
/// the underlying registry without restarting the process.
///
/// `load()` returns a snapshot `Arc<Registry>` that the caller can use for a
/// single request; the lock is held only long enough to clone the inner Arc.
#[derive(Clone)]
pub struct RegistryHandle {
    inner: Arc<std::sync::RwLock<Arc<Registry>>>,
}

impl RegistryHandle {
    pub fn new(registry: Registry) -> Self {
        Self {
            inner: Arc::new(std::sync::RwLock::new(Arc::new(registry))),
        }
    }

    pub fn load(&self) -> Arc<Registry> {
        self.inner.read().expect("registry lock poisoned").clone()
    }

    pub fn swap(&self, registry: Registry) {
        *self.inner.write().expect("registry lock poisoned") = Arc::new(registry);
    }
}

/// Outcome of a registry load: whatever loaded successfully, plus a list of
/// per-tracker failures. The caller decides whether to treat failures as
/// fatal (`tql doctor`) or warn-and-proceed (`tql mcp`).
#[derive(Debug)]
pub struct LoadReport {
    pub registry: Registry,
    pub failures: Vec<LoadFailure>,
}

impl LoadReport {
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Top-level error from [`load_dir`] when the root itself is unusable.
/// Per-tracker failures live inside [`LoadReport::failures`].
#[derive(Debug)]
pub enum RegistryError {
    /// The root path doesn't exist.
    RootMissing(PathBuf),
    /// The root exists but isn't a directory.
    RootNotDir(PathBuf),
    /// Couldn't read the root directory.
    Io(std::io::Error),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::RootMissing(p) => {
                write!(f, "trackers root does not exist: {}", p.display())
            }
            RegistryError::RootNotDir(p) => {
                write!(f, "trackers root is not a directory: {}", p.display())
            }
            RegistryError::Io(e) => write!(f, "io error reading trackers root: {e}"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Walk `root` (one level deep), load each subdirectory as a tracker against
/// `engine`, and return the assembled [`LoadReport`].
///
/// Hidden directories (leading `.`) are skipped. Non-directory entries at the
/// top level are silently ignored (lets users keep README.md, .git, etc.
/// alongside their trackers).
pub fn load_dir(root: &Path, engine: &Engine) -> Result<LoadReport, RegistryError> {
    if !root.exists() {
        return Err(RegistryError::RootMissing(root.to_path_buf()));
    }
    let meta = fs::metadata(root).map_err(RegistryError::Io)?;
    if !meta.is_dir() {
        return Err(RegistryError::RootNotDir(root.to_path_buf()));
    }

    let mut entries: Vec<PathBuf> = fs::read_dir(root)
        .map_err(RegistryError::Io)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| !n.starts_with('.'))
                    .unwrap_or(false)
        })
        .collect();
    entries.sort();

    let mut registry = Registry::new();
    let mut failures = Vec::new();

    // Track which directory contributed each name so DuplicateName can name
    // the existing one.
    let mut name_origin: BTreeMap<String, PathBuf> = BTreeMap::new();

    for dir in entries {
        match load_one(&dir, engine) {
            Ok(tracker) => {
                if let Some(existing) = name_origin.get(&tracker.manifest.name) {
                    failures.push(LoadFailure {
                        dir: dir.clone(),
                        error: TrackerLoadError::DuplicateName {
                            existing_dir: existing.clone(),
                        },
                    });
                    continue;
                }
                name_origin.insert(tracker.manifest.name.clone(), dir.clone());
                registry
                    .trackers
                    .insert(tracker.manifest.name.clone(), tracker);
            }
            Err(error) => failures.push(LoadFailure { dir, error }),
        }
    }

    Ok(LoadReport { registry, failures })
}

fn load_one(dir: &Path, engine: &Engine) -> Result<Tracker, TrackerLoadError> {
    let manifest_path = dir.join("manifest.toml");
    let script_path = dir.join("classify.rhai");

    if !manifest_path.exists() {
        return Err(TrackerLoadError::MissingManifest);
    }
    if !script_path.exists() {
        return Err(TrackerLoadError::MissingScript);
    }

    let manifest = manifest::load(&manifest_path).map_err(TrackerLoadError::Manifest)?;
    let source = fs::read_to_string(&script_path).map_err(TrackerLoadError::Io)?;
    let ast = host::compile(engine, &source).map_err(TrackerLoadError::Compile)?;

    Ok(Tracker {
        manifest,
        script: Arc::new(ast),
        dir: dir.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripting::sandbox::{build_engine, SandboxLimits};
    use std::io::Write;

    fn engine() -> Engine {
        build_engine(&SandboxLimits::default())
    }

    fn tmp_root() -> tempdir_lite::TempDir {
        tempdir_lite::TempDir::new("tql-registry-test")
    }

    fn write_tracker(root: &Path, name: &str, manifest: &str, script: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        let mut m = fs::File::create(dir.join("manifest.toml")).unwrap();
        m.write_all(manifest.as_bytes()).unwrap();
        let mut s = fs::File::create(dir.join("classify.rhai")).unwrap();
        s.write_all(script.as_bytes()).unwrap();
        dir
    }

    const GOOD_MANIFEST: &str = r#"
name = "myanonamouse"
canonical_category = "myanonamouse.net"
description = "MAM"

[[input]]
name = "url"
type = "string"
required = true
description = "url"
"#;

    const GOOD_SCRIPT: &str = r#"
fn classify(input) {
    #{ link_tags: ["link:x"], info_tags: [], warnings: [] }
}
"#;

    #[test]
    fn loads_a_single_good_tracker() {
        let tmp = tmp_root();
        write_tracker(tmp.path(), "mam", GOOD_MANIFEST, GOOD_SCRIPT);
        let report = load_dir(tmp.path(), &engine()).unwrap();
        assert!(report.ok(), "failures: {:?}", report.failures);
        assert_eq!(report.registry.len(), 1);
        let t = report.registry.get("myanonamouse").unwrap();
        assert_eq!(t.manifest.canonical_category, "myanonamouse.net");
        assert_eq!(t.dir.file_name().unwrap(), "mam");
    }

    #[test]
    fn loads_multiple_trackers_in_stable_order() {
        let tmp = tmp_root();
        let other_manifest = GOOD_MANIFEST.replace("myanonamouse", "redacted");
        write_tracker(tmp.path(), "z-mam", GOOD_MANIFEST, GOOD_SCRIPT);
        write_tracker(tmp.path(), "a-red", &other_manifest, GOOD_SCRIPT);
        let report = load_dir(tmp.path(), &engine()).unwrap();
        assert!(report.ok());
        let names: Vec<_> = report.registry.names().collect();
        // BTreeMap order is by manifest name, not directory name.
        assert_eq!(names, vec!["myanonamouse", "redacted"]);
    }

    #[test]
    fn missing_manifest_is_per_tracker_failure() {
        let tmp = tmp_root();
        let dir = tmp.path().join("broken");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("classify.rhai"), GOOD_SCRIPT).unwrap();
        // Add one good tracker too so we exercise "partial success".
        write_tracker(tmp.path(), "good", GOOD_MANIFEST, GOOD_SCRIPT);

        let report = load_dir(tmp.path(), &engine()).unwrap();
        assert_eq!(report.registry.len(), 1);
        assert_eq!(report.failures.len(), 1);
        assert!(matches!(
            report.failures[0].error,
            TrackerLoadError::MissingManifest
        ));
    }

    #[test]
    fn missing_script_is_per_tracker_failure() {
        let tmp = tmp_root();
        let dir = tmp.path().join("broken");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("manifest.toml"), GOOD_MANIFEST).unwrap();

        let report = load_dir(tmp.path(), &engine()).unwrap();
        assert_eq!(report.registry.len(), 0);
        assert!(matches!(
            report.failures[0].error,
            TrackerLoadError::MissingScript
        ));
    }

    #[test]
    fn manifest_parse_error_is_per_tracker_failure() {
        let tmp = tmp_root();
        write_tracker(tmp.path(), "broken", "this is not toml = = =", GOOD_SCRIPT);
        let report = load_dir(tmp.path(), &engine()).unwrap();
        assert!(report.registry.is_empty());
        assert!(matches!(
            report.failures[0].error,
            TrackerLoadError::Manifest(_)
        ));
    }

    #[test]
    fn script_compile_error_is_per_tracker_failure() {
        let tmp = tmp_root();
        write_tracker(
            tmp.path(),
            "broken",
            GOOD_MANIFEST,
            "fn classify(input) { this is not rhai }",
        );
        let report = load_dir(tmp.path(), &engine()).unwrap();
        assert!(report.registry.is_empty());
        assert!(matches!(
            report.failures[0].error,
            TrackerLoadError::Compile(_)
        ));
    }

    #[test]
    fn duplicate_name_keeps_first_reports_second() {
        let tmp = tmp_root();
        // Two directories, both with manifest name "myanonamouse".
        write_tracker(tmp.path(), "a-first", GOOD_MANIFEST, GOOD_SCRIPT);
        write_tracker(tmp.path(), "z-second", GOOD_MANIFEST, GOOD_SCRIPT);

        let report = load_dir(tmp.path(), &engine()).unwrap();
        assert_eq!(report.registry.len(), 1);
        assert_eq!(report.failures.len(), 1);
        // a-first sorts first alphabetically, so it wins.
        let winner = report.registry.get("myanonamouse").unwrap();
        assert!(winner.dir.ends_with("a-first"));
        match &report.failures[0].error {
            TrackerLoadError::DuplicateName { existing_dir } => {
                assert!(existing_dir.ends_with("a-first"));
            }
            other => panic!("expected DuplicateName, got {other:?}"),
        }
    }

    #[test]
    fn hidden_dirs_skipped() {
        let tmp = tmp_root();
        write_tracker(tmp.path(), ".hidden", GOOD_MANIFEST, GOOD_SCRIPT);
        write_tracker(tmp.path(), "good", GOOD_MANIFEST, GOOD_SCRIPT);
        let report = load_dir(tmp.path(), &engine()).unwrap();
        assert_eq!(report.registry.len(), 1);
    }

    #[test]
    fn non_dir_entries_at_root_ignored() {
        let tmp = tmp_root();
        fs::write(tmp.path().join("README.md"), "hi").unwrap();
        write_tracker(tmp.path(), "good", GOOD_MANIFEST, GOOD_SCRIPT);
        let report = load_dir(tmp.path(), &engine()).unwrap();
        assert!(report.ok());
        assert_eq!(report.registry.len(), 1);
    }

    #[test]
    fn missing_root_is_top_level_error() {
        let tmp = tmp_root();
        let bogus = tmp.path().join("no-such");
        let err = load_dir(&bogus, &engine()).unwrap_err();
        assert!(matches!(err, RegistryError::RootMissing(_)));
    }

    #[test]
    fn root_not_a_dir_is_top_level_error() {
        let tmp = tmp_root();
        let f = tmp.path().join("file");
        fs::write(&f, "").unwrap();
        let err = load_dir(&f, &engine()).unwrap_err();
        assert!(matches!(err, RegistryError::RootNotDir(_)));
    }

    #[test]
    fn empty_root_loads_empty_registry() {
        let tmp = tmp_root();
        let report = load_dir(tmp.path(), &engine()).unwrap();
        assert!(report.ok());
        assert!(report.registry.is_empty());
    }

    /// Loaded `Tracker` is usable end-to-end: feed its AST into `run_classify`.
    #[test]
    fn loaded_tracker_runs_classify() {
        let tmp = tmp_root();
        write_tracker(tmp.path(), "mam", GOOD_MANIFEST, GOOD_SCRIPT);
        let report = load_dir(tmp.path(), &engine()).unwrap();
        let t = report.registry.get("myanonamouse").unwrap();
        let out = host::run_classify(
            &engine(),
            &t.script,
            rhai::Map::new(),
            &t.manifest.canonical_category,
        )
        .unwrap();
        assert_eq!(out.link_tags, vec!["link:x".to_string()]);
    }

    // ---- tiny tempdir helper, to avoid a dev-dep --------------------------
    mod tempdir_lite {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);

        pub struct TempDir {
            path: PathBuf,
        }

        impl TempDir {
            pub fn new(prefix: &str) -> Self {
                let pid = std::process::id();
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0);
                let path = std::env::temp_dir().join(format!("{prefix}-{pid}-{nanos}-{n}"));
                std::fs::create_dir_all(&path).unwrap();
                Self { path }
            }
            pub fn path(&self) -> &Path {
                &self.path
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }
    }
}
