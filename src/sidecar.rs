//! Per-torrent sidecar JSON (DESIGN.md §14).
//!
//! Layout: `<library_root>/.metadata/<info_hash_v1>.json`.
//!
//! Writes go through a write-temp-then-rename dance while holding an exclusive
//! `flock` on an adjacent `.lock` file. Reads take a shared `flock` on the same
//! lock file. A missing sidecar means "fresh"; a malformed one is reported as
//! a parse error so callers can quarantine the torrent (DESIGN.md §14).
//!
//! The lock is held on a separate file (rather than the sidecar itself)
//! because the sidecar is replaced via `rename(2)` and a lock on a soon-to-be-
//! unlinked inode is not useful to subsequent readers.
//!
//! No timestamp generation lives here — callers pass in `created_at` /
//! `last_applied_at` strings. Keeps the module pure-ish and testable without
//! freezing time.
#![allow(dead_code)]
// Public surface is consumed by post_process / reconcile / sidecar_show in
// later legs; the warnings would be noise until then.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// Where the sidecar lives for a given info hash.
pub fn sidecar_path(library_root: &Path, info_hash_v1: &str) -> PathBuf {
    library_root
        .join(".metadata")
        .join(format!("{info_hash_v1}.json"))
}

fn lock_path(sidecar: &Path) -> PathBuf {
    let mut p = sidecar.to_path_buf();
    let name = sidecar
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    p.set_file_name(format!(".{name}.lock"));
    p
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    PostProcess,
    Reconcile,
    Manual,
    /// Reserved for cross-seed integration (DESIGN.md §14). Unused in v1.
    CrossSeed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkSite {
    pub relative_path: String,
    pub resolved_path: String,
    pub created_at: String,
    pub origin: Origin,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sidecar {
    pub schema_version: u32,
    pub info_hash_v1: String,
    #[serde(default)]
    pub info_hash_v2: Option<String>,
    pub name: String,
    pub category: String,
    pub content_path: String,
    pub is_directory: bool,
    pub size_bytes: u64,
    #[serde(default)]
    pub link_sites: Vec<LinkSite>,
    #[serde(default)]
    pub last_applied_tags: Vec<String>,
    #[serde(default)]
    pub last_applied_at: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum SidecarError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    UnsupportedSchema { found: u32, expected: u32 },
}

impl std::fmt::Display for SidecarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Parse(e) => write!(f, "parse: {e}"),
            Self::UnsupportedSchema { found, expected } => write!(
                f,
                "unsupported schema_version {found} (expected {expected})"
            ),
        }
    }
}

impl std::error::Error for SidecarError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Parse(e) => Some(e),
            Self::UnsupportedSchema { .. } => None,
        }
    }
}

impl From<std::io::Error> for SidecarError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for SidecarError {
    fn from(e: serde_json::Error) -> Self {
        Self::Parse(e)
    }
}

/// Read a sidecar at `path`. Returns `Ok(None)` if the file doesn't exist.
/// A malformed file surfaces as `Err(SidecarError::Parse)` — callers may
/// translate that into a quarantine.
pub fn read(path: &Path) -> Result<Option<Sidecar>, SidecarError> {
    if !path.exists() {
        return Ok(None);
    }
    // Shared lock on the adjacent .lock (created on demand) to coexist with
    // writers serializing on the same path.
    let lp = lock_path(path);
    if let Some(parent) = lp.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lp)?;
    lock.lock_shared()?;
    let _guard = LockGuard(lock);

    let mut f = File::open(path)?;
    let mut buf = String::new();
    f.read_to_string(&mut buf)?;
    let sc: Sidecar = serde_json::from_str(&buf)?;
    if sc.schema_version != SCHEMA_VERSION {
        return Err(SidecarError::UnsupportedSchema {
            found: sc.schema_version,
            expected: SCHEMA_VERSION,
        });
    }
    Ok(Some(sc))
}

/// Atomically write a sidecar to `path` under an exclusive lock. Creates the
/// `.metadata/` directory if missing.
pub fn write(path: &Path, sc: &Sidecar) -> Result<(), SidecarError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lp = lock_path(path);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lp)?;
    lock.lock_exclusive()?;
    let _guard = LockGuard(lock);

    // Pretty-print so humans inspecting the sidecar manually get a readable
    // file; size cost is trivial.
    let body = serde_json::to_vec_pretty(sc)?;

    let tmp = path.with_extension("json.tmp");
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

struct LockGuard(File);
impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn tmpdir(tag: &str) -> PathBuf {
        let mut d = env::temp_dir();
        d.push(format!(
            "tql-sidecar-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn sample() -> Sidecar {
        Sidecar {
            schema_version: SCHEMA_VERSION,
            info_hash_v1: "abc123".into(),
            info_hash_v2: None,
            name: "Some Torrent".into(),
            category: "myanonamouse.net".into(),
            content_path: "/data/torrents/myanonamouse.net/Some Torrent/".into(),
            is_directory: true,
            size_bytes: 4_760_000,
            link_sites: vec![LinkSite {
                relative_path: "Computer/Internet/Hamza Farooq".into(),
                resolved_path:
                    "/library/myanonamouse.net/Computer/Internet/Hamza Farooq/Some Torrent/".into(),
                created_at: "2026-05-10T12:34:56Z".into(),
                origin: Origin::PostProcess,
            }],
            last_applied_tags: vec!["link:Computer/Internet/Hamza Farooq".into()],
            last_applied_at: Some("2026-05-10T12:34:56Z".into()),
            warnings: vec![],
        }
    }

    #[test]
    fn sidecar_path_layout() {
        let p = sidecar_path(Path::new("/library"), "abc");
        assert_eq!(p, PathBuf::from("/library/.metadata/abc.json"));
    }

    #[test]
    fn read_missing_returns_none() {
        let d = tmpdir("missing");
        let p = sidecar_path(&d, "deadbeef");
        assert!(read(&p).unwrap().is_none());
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn round_trip() {
        let d = tmpdir("rt");
        let p = sidecar_path(&d, "abc123");
        let original = sample();
        write(&p, &original).unwrap();
        let back = read(&p).unwrap().unwrap();
        assert_eq!(original, back);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn write_creates_metadata_dir() {
        let d = tmpdir("mkdir");
        let p = sidecar_path(&d, "abc");
        assert!(!d.join(".metadata").exists());
        write(&p, &sample()).unwrap();
        assert!(d.join(".metadata").is_dir());
        assert!(p.is_file());
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn overwrite_is_atomic_rename() {
        // Sequential overwrite leaves no .tmp behind.
        let d = tmpdir("over");
        let p = sidecar_path(&d, "abc");
        let mut sc = sample();
        write(&p, &sc).unwrap();
        sc.name = "Renamed".into();
        write(&p, &sc).unwrap();
        let back = read(&p).unwrap().unwrap();
        assert_eq!(back.name, "Renamed");
        let leftover = p.with_extension("json.tmp");
        assert!(!leftover.exists(), "tmp file leaked: {leftover:?}");
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn malformed_surfaces_parse_error() {
        let d = tmpdir("bad");
        let p = sidecar_path(&d, "abc");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, b"{not json").unwrap();
        match read(&p) {
            Err(SidecarError::Parse(_)) => {}
            other => panic!("expected parse error, got {other:?}"),
        }
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn unsupported_schema_version_rejected() {
        let d = tmpdir("schema");
        let p = sidecar_path(&d, "abc");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut sc = sample();
        sc.schema_version = 999;
        // Bypass `write` (which doesn't validate on its end) so we control
        // the on-disk bytes.
        fs::write(&p, serde_json::to_vec(&sc).unwrap()).unwrap();
        match read(&p) {
            Err(SidecarError::UnsupportedSchema {
                found: 999,
                expected: 1,
            }) => {}
            other => panic!("expected unsupported schema, got {other:?}"),
        }
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn origin_serializes_snake_case() {
        let s = serde_json::to_string(&Origin::PostProcess).unwrap();
        assert_eq!(s, "\"post_process\"");
        let s = serde_json::to_string(&Origin::CrossSeed).unwrap();
        assert_eq!(s, "\"cross_seed\"");
    }
}
