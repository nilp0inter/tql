//! Linking primitives (DESIGN.md §9).
//!
//! For each desired link site `T` of a torrent with content path `S`:
//!
//! 1. If `T` already exists and already mirrors `S` (same inode for a single
//!    file, or every file under `T` is a hardlink of its counterpart under `S`
//!    for a directory) → idempotent success.
//! 2. Build at a sibling temp `T.tmp.<rand>` on the same filesystem.
//! 3. Single-file: `link(2)` `S` → temp.
//! 4. Multi-file: recreate the directory tree, `link(2)` each regular file,
//!    reproduce symlinks as symlinks.
//! 5. `rename(2)` temp → `T`.
//! 6. On any failure, remove the partial temp tree.
//!
//! `EXDEV` is a fatal configuration error; we never fall back to copy.
//!
//! Reflink stands in for `link(2)` when the configured strategy asks for it
//! and the filesystem supports it. From the sidecar's perspective the two are
//! equivalent — both share extents/inode with the seed.
//!
//! No filesystem mutation happens outside `link_to_site` / `unlink_site`.
#![allow(dead_code)]
// Public surface is consumed by post_process / reconcile in later legs.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Linux `EXDEV` — cross-device link not permitted.
const EXDEV: i32 = 18;

/// How the post-processor should materialize a link site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStrategy {
    /// Always `link(2)`. `EXDEV` is fatal.
    Hardlink,
    /// Always reflink (`copy_file_range`/`FICLONE`). Fails if the FS doesn't support it.
    Reflink,
    /// Try reflink, fall back to `link(2)`.
    ReflinkOrHardlink,
}

#[derive(Debug, Clone, Copy)]
pub struct LinkOpts {
    pub strategy: LinkStrategy,
}

impl Default for LinkOpts {
    fn default() -> Self {
        Self { strategy: LinkStrategy::Hardlink }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum LinkOutcome {
    /// `T` did not exist (or its parent didn't); we created it.
    Created,
    /// `T` already mirrored `S` correctly. No-op.
    AlreadyCorrect,
}

#[derive(Debug)]
pub enum LinkError {
    /// Source path missing or unreadable.
    SourceMissing(PathBuf, io::Error),
    /// Target exists but is not a correct mirror of the source.
    TargetConflict(PathBuf),
    /// `link(2)` returned `EXDEV`. Hardlinks can't cross filesystems and we
    /// refuse to fall back to copy.
    CrossDevice { source: PathBuf, target: PathBuf },
    /// Reflink unsupported by the filesystem and strategy was `Reflink`.
    ReflinkUnsupported(PathBuf),
    /// Found a file type we don't know how to mirror (block/char/socket/fifo).
    UnsupportedFileType(PathBuf),
    Io(PathBuf, io::Error),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceMissing(p, e) => write!(f, "source missing: {}: {e}", p.display()),
            Self::TargetConflict(p) => write!(f, "target exists and is not a correct mirror: {}", p.display()),
            Self::CrossDevice { source, target } => write!(
                f,
                "EXDEV: cannot hardlink across filesystems: {} -> {}",
                source.display(),
                target.display()
            ),
            Self::ReflinkUnsupported(p) => write!(f, "reflink unsupported on: {}", p.display()),
            Self::UnsupportedFileType(p) => write!(f, "unsupported file type: {}", p.display()),
            Self::Io(p, e) => write!(f, "io: {}: {e}", p.display()),
        }
    }
}

impl std::error::Error for LinkError {}

#[derive(Debug)]
pub enum UnlinkError {
    Io(PathBuf, io::Error),
    /// Walked up to the configured stop boundary; refused to delete it.
    HitStopBoundary(PathBuf),
}

impl std::fmt::Display for UnlinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(p, e) => write!(f, "io: {}: {e}", p.display()),
            Self::HitStopBoundary(p) => write!(f, "stop boundary: {}", p.display()),
        }
    }
}

impl std::error::Error for UnlinkError {}

/// Materialize `target` as a mirror of `source` per §9.
///
/// Returns `AlreadyCorrect` if `target` already mirrors `source` (idempotent
/// re-run); returns `Created` on a fresh build.
pub fn link_to_site(
    source: &Path,
    target: &Path,
    opts: &LinkOpts,
) -> Result<LinkOutcome, LinkError> {
    let s_meta = fs::symlink_metadata(source)
        .map_err(|e| LinkError::SourceMissing(source.to_path_buf(), e))?;

    if target.exists() {
        if mirrors(source, target, &s_meta)? {
            return Ok(LinkOutcome::AlreadyCorrect);
        }
        return Err(LinkError::TargetConflict(target.to_path_buf()));
    }

    let parent = target.parent().ok_or_else(|| {
        LinkError::Io(
            target.to_path_buf(),
            io::Error::new(io::ErrorKind::InvalidInput, "target has no parent"),
        )
    })?;
    fs::create_dir_all(parent).map_err(|e| LinkError::Io(parent.to_path_buf(), e))?;

    let tmp = sibling_tmp(target);
    // Defensive: a stray temp from a previous crash blocks us. Clean it up
    // before we start. (Won't race because the name is process+counter unique.)
    let _ = remove_path(&tmp);

    let build = if s_meta.file_type().is_dir() {
        build_dir(source, &tmp, opts)
    } else {
        build_one(source, &tmp, &s_meta, opts)
    };

    if let Err(e) = build {
        let _ = remove_path(&tmp);
        return Err(e);
    }

    fs::rename(&tmp, target).map_err(|e| {
        let _ = remove_path(&tmp);
        LinkError::Io(target.to_path_buf(), e)
    })?;

    Ok(LinkOutcome::Created)
}

/// Remove `target`, then walk upward removing newly-empty directories until
/// hitting `stop_at` (exclusive — `stop_at` is never deleted).
///
/// `stop_at` is typically `<library_root>/<category>/`.
pub fn unlink_site(target: &Path, stop_at: &Path) -> Result<(), UnlinkError> {
    match fs::symlink_metadata(target) {
        Ok(m) if m.file_type().is_dir() => {
            fs::remove_dir_all(target)
                .map_err(|e| UnlinkError::Io(target.to_path_buf(), e))?;
        }
        Ok(_) => {
            fs::remove_file(target)
                .map_err(|e| UnlinkError::Io(target.to_path_buf(), e))?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(UnlinkError::Io(target.to_path_buf(), e)),
    }

    let stop = stop_at.canonicalize().unwrap_or_else(|_| stop_at.to_path_buf());
    let mut cur = target.parent().map(Path::to_path_buf);
    while let Some(dir) = cur {
        let dir_canon = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if dir_canon == stop {
            break;
        }
        // Refuse to ascend above the stop boundary even if canonicalization
        // disagreed; reaching the filesystem root is a hard stop too.
        if !dir_canon.starts_with(&stop) {
            break;
        }
        match fs::read_dir(&dir) {
            Ok(mut it) => {
                if it.next().is_some() {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Err(e) => return Err(UnlinkError::Io(dir, e)),
        }
        if let Err(e) = fs::remove_dir(&dir) {
            if e.kind() != io::ErrorKind::NotFound {
                return Err(UnlinkError::Io(dir, e));
            }
        }
        cur = dir.parent().map(Path::to_path_buf);
    }
    Ok(())
}

// -- internals ----------------------------------------------------------------

fn build_one(
    source: &Path,
    target: &Path,
    s_meta: &fs::Metadata,
    opts: &LinkOpts,
) -> Result<(), LinkError> {
    let ft = s_meta.file_type();
    if ft.is_symlink() {
        let link_target = fs::read_link(source)
            .map_err(|e| LinkError::Io(source.to_path_buf(), e))?;
        std::os::unix::fs::symlink(&link_target, target)
            .map_err(|e| LinkError::Io(target.to_path_buf(), e))?;
        return Ok(());
    }
    if !ft.is_file() {
        return Err(LinkError::UnsupportedFileType(source.to_path_buf()));
    }
    materialize_file(source, target, opts)
}

fn build_dir(source: &Path, target: &Path, opts: &LinkOpts) -> Result<(), LinkError> {
    fs::create_dir(target).map_err(|e| LinkError::Io(target.to_path_buf(), e))?;
    for entry in fs::read_dir(source).map_err(|e| LinkError::Io(source.to_path_buf(), e))? {
        let entry = entry.map_err(|e| LinkError::Io(source.to_path_buf(), e))?;
        let s = entry.path();
        let t = target.join(entry.file_name());
        let m = entry
            .metadata()
            .map_err(|e| LinkError::Io(s.clone(), e))?;
        let ft = m.file_type();
        if ft.is_dir() {
            build_dir(&s, &t, opts)?;
        } else if ft.is_symlink() {
            let link_target = fs::read_link(&s).map_err(|e| LinkError::Io(s.clone(), e))?;
            std::os::unix::fs::symlink(&link_target, &t)
                .map_err(|e| LinkError::Io(t.clone(), e))?;
        } else if ft.is_file() {
            materialize_file(&s, &t, opts)?;
        } else {
            return Err(LinkError::UnsupportedFileType(s));
        }
    }
    Ok(())
}

/// `link(2)` or reflink one regular file, per the configured strategy.
fn materialize_file(source: &Path, target: &Path, opts: &LinkOpts) -> Result<(), LinkError> {
    match opts.strategy {
        LinkStrategy::Hardlink => hardlink(source, target),
        LinkStrategy::Reflink => reflink(source, target).map_err(|e| match e {
            ReflinkErr::Unsupported => LinkError::ReflinkUnsupported(source.to_path_buf()),
            ReflinkErr::Io(e) => LinkError::Io(target.to_path_buf(), e),
        }),
        LinkStrategy::ReflinkOrHardlink => match reflink(source, target) {
            Ok(()) => Ok(()),
            Err(_) => hardlink(source, target),
        },
    }
}

fn hardlink(source: &Path, target: &Path) -> Result<(), LinkError> {
    fs::hard_link(source, target).map_err(|e| {
        if e.raw_os_error() == Some(EXDEV) {
            LinkError::CrossDevice {
                source: source.to_path_buf(),
                target: target.to_path_buf(),
            }
        } else {
            LinkError::Io(target.to_path_buf(), e)
        }
    })
}

enum ReflinkErr {
    Unsupported,
    Io(io::Error),
}

fn reflink(source: &Path, target: &Path) -> Result<(), ReflinkErr> {
    match reflink_copy::reflink(source, target) {
        Ok(()) => Ok(()),
        Err(e) => {
            // reflink-copy surfaces "filesystem doesn't support it" as a
            // distinct error kind on some platforms, but on Linux it's just an
            // io::Error with raw os error EOPNOTSUPP (95) / ENOTSUP / EINVAL.
            let raw = e.raw_os_error();
            if matches!(raw, Some(95) | Some(38) | Some(22)) {
                Err(ReflinkErr::Unsupported)
            } else {
                Err(ReflinkErr::Io(e))
            }
        }
    }
}

/// Does `target` already mirror `source`? Single-file: inode equality.
/// Directory: every regular file under `target` is a hardlink of its
/// counterpart under `source`; symlinks match by readlink; directory tree
/// matches structurally.
fn mirrors(source: &Path, target: &Path, s_meta: &fs::Metadata) -> Result<bool, LinkError> {
    let t_meta = match fs::symlink_metadata(target) {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };
    let s_ft = s_meta.file_type();
    let t_ft = t_meta.file_type();
    if s_ft.is_dir() != t_ft.is_dir() {
        return Ok(false);
    }
    if s_ft.is_file() && t_ft.is_file() {
        return Ok(same_inode(s_meta, &t_meta));
    }
    if s_ft.is_dir() && t_ft.is_dir() {
        return mirrors_dir(source, target);
    }
    Ok(false)
}

fn mirrors_dir(source: &Path, target: &Path) -> Result<bool, LinkError> {
    let mut s_names: Vec<_> = collect_names(source)?;
    let mut t_names: Vec<_> = collect_names(target)?;
    s_names.sort();
    t_names.sort();
    if s_names != t_names {
        return Ok(false);
    }
    for name in s_names {
        let s = source.join(&name);
        let t = target.join(&name);
        let sm = fs::symlink_metadata(&s).map_err(|e| LinkError::Io(s.clone(), e))?;
        let tm = fs::symlink_metadata(&t).map_err(|e| LinkError::Io(t.clone(), e))?;
        let sft = sm.file_type();
        let tft = tm.file_type();
        if sft.is_dir() && tft.is_dir() {
            if !mirrors_dir(&s, &t)? {
                return Ok(false);
            }
        } else if sft.is_symlink() && tft.is_symlink() {
            let st = fs::read_link(&s).map_err(|e| LinkError::Io(s.clone(), e))?;
            let tt = fs::read_link(&t).map_err(|e| LinkError::Io(t.clone(), e))?;
            if st != tt {
                return Ok(false);
            }
        } else if sft.is_file() && tft.is_file() {
            if !same_inode(&sm, &tm) {
                return Ok(false);
            }
        } else {
            return Ok(false);
        }
    }
    Ok(true)
}

fn collect_names(dir: &Path) -> Result<Vec<std::ffi::OsString>, LinkError> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| LinkError::Io(dir.to_path_buf(), e))? {
        let entry = entry.map_err(|e| LinkError::Io(dir.to_path_buf(), e))?;
        out.push(entry.file_name());
    }
    Ok(out)
}

fn same_inode(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

fn sibling_tmp(target: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let name = target.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{name}.tmp.{pid}.{nanos}.{n}"))
}

fn remove_path(p: &Path) -> io::Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_dir() => fs::remove_dir_all(p),
        Ok(_) => fs::remove_file(p),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// -- tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir();
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let pid = std::process::id();
        let p = base.join(format!("tql-linking-test.{pid}.{nanos}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_file(p: &Path, body: &[u8]) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(p).unwrap();
        f.write_all(body).unwrap();
    }

    #[test]
    fn single_file_hardlink_creates_and_is_idempotent() {
        let dir = tmpdir();
        let s = dir.join("seed/file.bin");
        write_file(&s, b"abc");
        let t = dir.join("lib/cat/proj/file.bin");
        let opts = LinkOpts { strategy: LinkStrategy::Hardlink };

        assert_eq!(link_to_site(&s, &t, &opts).unwrap(), LinkOutcome::Created);
        // Second run: same inode — idempotent success.
        assert_eq!(link_to_site(&s, &t, &opts).unwrap(), LinkOutcome::AlreadyCorrect);
        let sm = fs::metadata(&s).unwrap();
        let tm = fs::metadata(&t).unwrap();
        assert!(same_inode(&sm, &tm));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn target_conflict_when_not_hardlink() {
        let dir = tmpdir();
        let s = dir.join("seed/a.bin");
        write_file(&s, b"abc");
        let t = dir.join("lib/cat/a.bin");
        // Pre-existing unrelated file at T.
        write_file(&t, b"different content");
        let err = link_to_site(&s, &t, &LinkOpts::default()).unwrap_err();
        assert!(matches!(err, LinkError::TargetConflict(_)));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn directory_torrent_hardlinks_each_file() {
        let dir = tmpdir();
        let s = dir.join("seed/proj");
        write_file(&s.join("a.txt"), b"alpha");
        write_file(&s.join("sub/b.txt"), b"bravo");
        // Symlink inside the source: should be reproduced as a symlink.
        std::os::unix::fs::symlink("a.txt", s.join("link-to-a")).unwrap();

        let t = dir.join("lib/cat/proj");
        let opts = LinkOpts { strategy: LinkStrategy::Hardlink };

        assert_eq!(link_to_site(&s, &t, &opts).unwrap(), LinkOutcome::Created);
        // Regular files share inodes.
        let sa = fs::metadata(s.join("a.txt")).unwrap();
        let ta = fs::metadata(t.join("a.txt")).unwrap();
        assert!(same_inode(&sa, &ta));
        let sb = fs::metadata(s.join("sub/b.txt")).unwrap();
        let tb = fs::metadata(t.join("sub/b.txt")).unwrap();
        assert!(same_inode(&sb, &tb));
        // Symlink preserved as a symlink, with the same target.
        let link_meta = fs::symlink_metadata(t.join("link-to-a")).unwrap();
        assert!(link_meta.file_type().is_symlink());
        assert_eq!(fs::read_link(t.join("link-to-a")).unwrap(), Path::new("a.txt"));

        // Idempotent re-run.
        assert_eq!(link_to_site(&s, &t, &opts).unwrap(), LinkOutcome::AlreadyCorrect);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn directory_target_conflict_when_extra_file() {
        let dir = tmpdir();
        let s = dir.join("seed/proj");
        write_file(&s.join("a.txt"), b"alpha");
        let t = dir.join("lib/cat/proj");
        // Create T as a directory with an extra unrelated file.
        write_file(&t.join("a.txt"), b"alpha");
        write_file(&t.join("extra.txt"), b"surprise");
        let err = link_to_site(&s, &t, &LinkOpts::default()).unwrap_err();
        assert!(matches!(err, LinkError::TargetConflict(_)));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_temp_left_after_success() {
        let dir = tmpdir();
        let s = dir.join("seed/a.bin");
        write_file(&s, b"abc");
        let t = dir.join("lib/cat/a.bin");
        link_to_site(&s, &t, &LinkOpts::default()).unwrap();
        // Scan parent for stray .tmp.* siblings.
        let parent = t.parent().unwrap();
        for entry in fs::read_dir(parent).unwrap() {
            let n = entry.unwrap().file_name();
            let s = n.to_string_lossy();
            assert!(!s.contains(".tmp."), "stray temp left: {s}");
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unlink_prunes_empty_parents_up_to_stop() {
        let dir = tmpdir();
        let stop = dir.join("lib/cat");
        let t = stop.join("proj/sub/a.bin");
        write_file(&t, b"x");
        unlink_site(&t, &stop).unwrap();
        // proj/ and proj/sub/ removed.
        assert!(!dir.join("lib/cat/proj").exists());
        // Stop boundary preserved.
        assert!(stop.exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unlink_stops_when_sibling_present() {
        let dir = tmpdir();
        let stop = dir.join("lib/cat");
        let t = stop.join("proj/a.bin");
        let keep = stop.join("proj/b.bin");
        write_file(&t, b"x");
        write_file(&keep, b"y");
        unlink_site(&t, &stop).unwrap();
        // proj/ NOT pruned because b.bin remains.
        assert!(keep.exists());
        assert!(stop.join("proj").exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unlink_missing_target_is_ok() {
        let dir = tmpdir();
        let stop = dir.join("lib/cat");
        fs::create_dir_all(&stop).unwrap();
        let t = stop.join("never-existed");
        unlink_site(&t, &stop).unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unlink_directory_target() {
        let dir = tmpdir();
        let stop = dir.join("lib/cat");
        let t = stop.join("proj");
        write_file(&t.join("a.bin"), b"x");
        write_file(&t.join("sub/b.bin"), b"y");
        unlink_site(&t, &stop).unwrap();
        assert!(!t.exists());
        assert!(stop.exists());
        fs::remove_dir_all(&dir).ok();
    }
}
