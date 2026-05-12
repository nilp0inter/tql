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
//! Atomic drain (`drain`) renames the spool to a sibling `.flushing` file
//! under an exclusive flock so concurrent `enqueue` calls land in a fresh
//! spool. The drainer is then free to parse and dispatch without blocking
//! producers. On partial failure the caller appends unsent events back to
//! the spool via `requeue` (best-effort ordering — newer enqueues may land
//! before the requeued tail; acceptable for notifications).

#![allow(dead_code)]

pub mod render;
pub mod telegram;

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
    let mut f = OpenOptions::new().create(true).append(true).open(spool)?;
    f.lock_exclusive()?;
    let mut line =
        serde_json::to_vec(event).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    line.push(b'\n');
    let res = f.write_all(&line).and_then(|_| f.flush());
    let _ = FileExt::unlock(&f);
    res
}

/// Sibling path holding events that are mid-flush. Atomically renamed from
/// the spool by `drain`, then deleted by `commit_drain` once dispatched.
pub fn flushing_path(spool: &Path) -> PathBuf {
    let mut name = spool
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("notify.spool"));
    name.push(".flushing");
    spool.with_file_name(name)
}

/// Atomically move the spool aside and return its contents. Caller is
/// responsible for calling `commit_drain` (on success) or `requeue` (on
/// partial failure) to clean up the flushing file.
///
/// If the spool does not exist the result is `Ok(Vec::new())` and no
/// flushing file is created. If a previous run left a flushing file behind,
/// its contents are returned and a fresh rename merges any newly-enqueued
/// events on top.
pub fn drain(spool: &Path) -> io::Result<Vec<Event>> {
    let flushing = flushing_path(spool);
    // Salvage any prior flushing file first; its events are older than the
    // current spool, so they come first in the returned vec.
    let mut events = if flushing.exists() {
        read_all(&flushing)?
    } else {
        Vec::new()
    };
    match OpenOptions::new().read(true).write(true).open(spool) {
        Ok(f) => {
            f.lock_exclusive()?;
            // While locked, rename to flushing. Even if the spool was
            // recreated since the prior `exists` check, the rename
            // atomically captures whatever it points at right now.
            let res = fs::rename(spool, &flushing);
            let _ = FileExt::unlock(&f);
            res?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Nothing fresh to drain; return whatever the prior flushing
            // file had (possibly empty).
            return Ok(events);
        }
        Err(e) => return Err(e),
    }
    events.extend(read_all(&flushing)?);
    Ok(events)
}

/// Successful drain → delete the flushing file. Missing file is OK.
pub fn commit_drain(spool: &Path) -> io::Result<()> {
    let flushing = flushing_path(spool);
    match fs::remove_file(&flushing) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Partial failure → append `unsent` events back to the spool (best-effort
/// ordering) and clear the flushing file. Producers continuing to enqueue
/// while we ran will already have written to a fresh spool; their events
/// stay ahead of the requeued tail.
pub fn requeue(spool: &Path, unsent: &[Event]) -> io::Result<()> {
    for ev in unsent {
        enqueue(spool, ev)?;
    }
    commit_drain(spool)
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
            d.push(format!(
                "tql-notify-{}-{}-{}",
                tag,
                std::process::id(),
                nanos
            ));
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
    fn drain_moves_events_aside_and_clears_spool() {
        let d = TempDir::new("drain");
        let spool = d.path().join("notify.spool");
        enqueue(&spool, &sample("a")).unwrap();
        enqueue(&spool, &sample("b")).unwrap();
        let events = drain(&spool).unwrap();
        assert_eq!(events.len(), 2);
        assert!(!spool.exists());
        assert!(flushing_path(&spool).exists());
        commit_drain(&spool).unwrap();
        assert!(!flushing_path(&spool).exists());
    }

    #[test]
    fn drain_missing_spool_is_empty() {
        let d = TempDir::new("drain-empty");
        let spool = d.path().join("notify.spool");
        assert!(drain(&spool).unwrap().is_empty());
        assert!(!flushing_path(&spool).exists());
    }

    #[test]
    fn drain_recovers_prior_flushing_file() {
        let d = TempDir::new("drain-recover");
        let spool = d.path().join("notify.spool");
        // Simulate a prior drainer that crashed mid-flight: a flushing
        // file with one event.
        let flushing = flushing_path(&spool);
        enqueue(&flushing, &sample("old")).unwrap();
        // Plus a fresh spool with new events.
        enqueue(&spool, &sample("new")).unwrap();
        let events = drain(&spool).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].info_hash_v1, "old");
        assert_eq!(events[1].info_hash_v1, "new");
    }

    #[test]
    fn requeue_writes_tail_back_and_clears_flushing() {
        let d = TempDir::new("requeue");
        let spool = d.path().join("notify.spool");
        enqueue(&spool, &sample("a")).unwrap();
        enqueue(&spool, &sample("b")).unwrap();
        let events = drain(&spool).unwrap();
        // Pretend the first sent OK, second did not.
        requeue(&spool, &events[1..]).unwrap();
        assert!(!flushing_path(&spool).exists());
        let leftover = read_all(&spool).unwrap();
        assert_eq!(leftover.len(), 1);
        assert_eq!(leftover[0].info_hash_v1, "b");
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
