//! PID files for long-running `tql` servers (DESIGN.md §7 `tql reload`).
//!
//! Directory selection order:
//!   1. `$TQL_RUN_DIR` (test escape hatch / explicit override).
//!   2. `$XDG_RUNTIME_DIR/tql/` (typical for unprivileged services).
//!   3. `/run/tql/` (when running as root and `/run/` is writable).
//!   4. `/tmp/tql-<uid>/` (last-resort fallback).
//!
//! Each server role (`api`, `mcp`) gets its own `<role>.pid` file holding the
//! decimal PID of the live process. `read` returns `None` when the file is
//! absent or the PID is stale (i.e., no process with that PID is alive).
//! Stale files are removed lazily on `read` to avoid spurious "running" state.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Parse,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "i/o: {e}"),
            Error::Parse => write!(f, "pid file does not contain a numeric PID"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Resolve the directory PID files live in, creating it if necessary.
pub fn dir() -> Result<PathBuf, Error> {
    let candidate = pick_dir();
    fs::create_dir_all(&candidate)?;
    Ok(candidate)
}

fn pick_dir() -> PathBuf {
    if let Ok(p) = std::env::var("TQL_RUN_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(p) = std::env::var("XDG_RUNTIME_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p).join("tql");
        }
    }
    let euid = unsafe { libc::geteuid() };
    if euid == 0 && Path::new("/run").is_dir() {
        return PathBuf::from("/run/tql");
    }
    PathBuf::from(format!("/tmp/tql-{euid}"))
}

/// Absolute path for the given role's PID file.
pub fn path_for(role: &str) -> Result<PathBuf, Error> {
    Ok(dir()?.join(format!("{role}.pid")))
}

/// Write the current process's PID to the role's PID file. Overwrites any
/// existing content (callers should check first if they want to be polite).
pub fn write(role: &str) -> Result<PathBuf, Error> {
    let path = path_for(role)?;
    let pid = std::process::id();
    let tmp = path.with_extension("pid.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        write!(f, "{pid}")?;
        f.flush()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Read the role's PID file. Returns `Ok(None)` when absent or stale. Stale
/// files are removed (best-effort) so the caller sees a consistent view.
pub fn read(role: &str) -> Result<Option<u32>, Error> {
    let path = path_for(role)?;
    let raw = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    };
    let pid: u32 = raw.trim().parse().map_err(|_| Error::Parse)?;
    if !pid_alive(pid) {
        let _ = fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(pid))
}

/// Remove the role's PID file. Missing is not an error.
pub fn remove(role: &str) -> Result<(), Error> {
    let path = path_for(role)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Send SIGHUP to `pid`. Returns whether the signal was delivered.
pub fn send_sighup(pid: u32) -> Result<(), Error> {
    let rc = unsafe { libc::kill(pid as i32, libc::SIGHUP) };
    if rc == 0 {
        Ok(())
    } else {
        Err(Error::Io(std::io::Error::last_os_error()))
    }
}

fn pid_alive(pid: u32) -> bool {
    // `kill(pid, 0)` returns 0 if the process exists and we may signal it.
    // EPERM means it exists but we lack permission — still "alive" for our
    // purposes. ESRCH means it's gone.
    let rc = unsafe { libc::kill(pid as i32, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated_dir() -> tempdir_lite::TempDir {
        let d = tempdir_lite::TempDir::new("tql-pidfile");
        std::env::set_var("TQL_RUN_DIR", d.path());
        d
    }

    // The tests share the global `TQL_RUN_DIR` env var, so serialize them.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        M.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn read_absent_returns_none() {
        let _g = lock();
        let _d = isolated_dir();
        assert!(read("nope").unwrap().is_none());
    }

    #[test]
    fn write_then_read_roundtrip() {
        let _g = lock();
        let _d = isolated_dir();
        let path = write("api").unwrap();
        assert!(path.ends_with("api.pid"));
        let pid = read("api").unwrap().unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn read_stale_pid_returns_none_and_removes_file() {
        let _g = lock();
        let d = isolated_dir();
        // Pick a PID that's almost certainly dead. Picking the max-i32 keeps us
        // out of any conceivable live process range.
        let path = d.path().join("ghost.pid");
        std::fs::write(&path, "2147483646").unwrap();
        assert!(read("ghost").unwrap().is_none());
        assert!(!path.exists());
    }

    #[test]
    fn remove_is_idempotent() {
        let _g = lock();
        let _d = isolated_dir();
        remove("missing").unwrap();
        write("api").unwrap();
        remove("api").unwrap();
        assert!(read("api").unwrap().is_none());
    }

    #[test]
    fn read_garbage_yields_parse_error() {
        let _g = lock();
        let d = isolated_dir();
        std::fs::write(d.path().join("api.pid"), "not-a-pid").unwrap();
        assert!(matches!(read("api"), Err(Error::Parse)));
    }

    mod tempdir_lite {
        use std::path::{Path, PathBuf};

        pub struct TempDir {
            path: PathBuf,
        }

        impl TempDir {
            pub fn new(prefix: &str) -> Self {
                let base = std::env::temp_dir();
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let pid = std::process::id();
                let path = base.join(format!("{prefix}-{pid}-{nanos}"));
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
