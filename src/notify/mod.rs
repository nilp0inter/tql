//! Notification spool (DESIGN.md §15).
//!
//! The post-processor enqueues a JSONL event per torrent into a file-backed
//! spool. A separate `tql notify-flush` (later leg) drains it, debounces,
//! batches and dispatches to configured backends (Telegram by default).
//!
//! This module only owns the on-disk primitive: append-only writes under an
//! exclusive `flock` so concurrent post-process invocations cannot interleave
//! partial lines.
//!
//! Backends, debounce/batch policy and the `notify-flush` CLI land in a
//! follow-up sub-leg.

#![allow(dead_code)]

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

/// One spool entry. Mirrors what an operator needs to render a notification
/// without re-reading the sidecar: name, category, hash, what changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub schema_version: u32,
    pub ts: String,
    pub info_hash_v1: String,
    pub name: String,
    pub category: String,
    /// Relative paths newly hardlinked into the library.
    #[serde(default)]
    pub link_sites_added: Vec<String>,
    /// Relative paths removed from the library.
    #[serde(default)]
    pub link_sites_removed: Vec<String>,
    /// Per-event warnings (validation hits, soft caps, link failures).
    #[serde(default)]
    pub warnings: Vec<String>,
}

pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Default spool path: `<library_root>/.metadata/notify.spool`.
pub fn default_spool_path(library_root: &Path) -> PathBuf {
    library_root.join(".metadata").join("notify.spool")
}

/// Append a single JSON line to the spool under an exclusive `flock`.
///
/// Creates parent directories and the file as needed. The lock guards against
/// torn writes when two post-process invocations finish at the same time.
pub fn enqueue(spool: &Path, event: &Event) -> io::Result<()> {
    if let Some(parent) = spool.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(spool)?;
    f.lock_exclusive()?;
    let mut line = serde_json::to_vec(event)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    line.push(b'\n');
    let res = f.write_all(&line).and_then(|_| f.flush());
    let _ = FileExt::unlock(&f);
    res
}

/// Read every event line from the spool. Malformed lines are returned as
/// errors so the drainer can decide what to do.
pub fn read_all(spool: &Path) -> io::Result<Vec<Event>> {
    let f = match OpenOptions::new().read(true).open(spool) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    f.lock_shared()?;
    let mut out = Vec::new();
    let reader = BufReader::new(&f);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let ev: Event = serde_json::from_str(&line)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        out.push(ev);
    }
    let _ = FileExt::unlock(&f);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut d = std::env::temp_dir();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            d.push(format!("tql-notify-{}-{}-{}", tag, std::process::id(), nanos));
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

    fn sample(hash: &str) -> Event {
        Event {
            schema_version: EVENT_SCHEMA_VERSION,
            ts: "2026-05-11T12:00:00Z".into(),
            info_hash_v1: hash.into(),
            name: "Some Book".into(),
            category: "tracker.tld".into(),
            link_sites_added: vec!["Computer/Hamza".into()],
            link_sites_removed: vec![],
            warnings: vec![],
        }
    }

    #[test]
    fn default_path_under_metadata() {
        let p = default_spool_path(Path::new("/data/lib"));
        assert_eq!(p, Path::new("/data/lib/.metadata/notify.spool"));
    }

    #[test]
    fn enqueue_then_read_roundtrip() {
        let d = TempDir::new("rt");
        let spool = d.path().join("notify.spool");
        let a = sample("aaaa");
        let b = sample("bbbb");
        enqueue(&spool, &a).unwrap();
        enqueue(&spool, &b).unwrap();
        let events = read_all(&spool).unwrap();
        assert_eq!(events, vec![a, b]);
    }

    #[test]
    fn read_missing_is_empty() {
        let d = TempDir::new("missing");
        let spool = d.path().join("nope.spool");
        assert_eq!(read_all(&spool).unwrap(), Vec::<Event>::new());
    }

    #[test]
    fn enqueue_creates_parent_dirs() {
        let d = TempDir::new("mkparent");
        let spool = d.path().join("a/b/c/notify.spool");
        enqueue(&spool, &sample("x")).unwrap();
        assert!(spool.exists());
    }

    #[test]
    fn malformed_line_surfaces_error() {
        let d = TempDir::new("bad");
        let spool = d.path().join("notify.spool");
        enqueue(&spool, &sample("ok")).unwrap();
        // Append garbage manually.
        let mut f = OpenOptions::new().append(true).open(&spool).unwrap();
        f.write_all(b"{not json\n").unwrap();
        let err = read_all(&spool).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn appends_one_line_per_event() {
        let d = TempDir::new("lines");
        let spool = d.path().join("notify.spool");
        for i in 0..5 {
            enqueue(&spool, &sample(&format!("h{i}"))).unwrap();
        }
        let raw = fs::read_to_string(&spool).unwrap();
        assert_eq!(raw.lines().count(), 5);
        for line in raw.lines() {
            // Each line must parse as a single Event.
            let _: Event = serde_json::from_str(line).unwrap();
        }
    }

    #[test]
    fn concurrent_enqueue_does_not_interleave() {
        let d = TempDir::new("concurrent");
        let spool = std::sync::Arc::new(d.path().join("notify.spool"));
        let mut handles = Vec::new();
        for t in 0..4 {
            let spool = spool.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..20 {
                    enqueue(&spool, &sample(&format!("t{t}-i{i}"))).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let events = read_all(&spool).unwrap();
        assert_eq!(events.len(), 4 * 20);
    }
}
