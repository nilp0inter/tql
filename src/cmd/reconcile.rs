//! `tql reconcile` — periodic safety net (DESIGN.md §7).
//!
//! Walks every qBittorrent torrent and runs the same diff/apply pipeline as
//! `tql post-process` against each one. Optional filters narrow the set:
//!
//! - `--torrent <hash>` limits to a single info hash.
//! - `--category <name>` limits to one tracker category.
//! - `--dry-run` computes diffs but performs no filesystem writes.
//!
//! Concurrency: per-torrent pipelines run on the tokio blocking pool throttled
//! by a semaphore of size `cfg.reconcile.parallelism` (DESIGN.md §11). The
//! per-hash flock inside `post_process::process_with_cfg` keeps two concurrent
//! workers from clobbering the same sidecar; the semaphore caps global fan-out.
//! Outcomes are gathered and printed in input order so the summary stays
//! deterministic regardless of finish order.
//!
//! Exit code: 0 when every torrent's pipeline ran to a sidecar (or planned
//! diff). 1 if any torrent aborted, or if connecting to qBittorrent failed.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;

use crate::cmd::post_process;
use crate::config;
use crate::qbit;
use crate::qbit::types::{TorrentInfo, TorrentsInfoQuery};

#[derive(Parser, Debug)]
pub struct Args {
    /// Print intended diff but make no changes.
    #[arg(long)]
    pub dry_run: bool,
    /// Limit to a single torrent.
    #[arg(long, value_name = "INFO_HASH")]
    pub torrent: Option<String>,
    /// Limit to a single category (tracker).
    #[arg(long, value_name = "NAME")]
    pub category: Option<String>,
    /// Optional config-file override.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<(), u8> {
    match do_run(&args) {
        Ok(summary) => {
            summary.print();
            if summary.aborted > 0 {
                Err(1)
            } else {
                Ok(())
            }
        }
        Err(msg) => {
            eprintln!("tql reconcile: {msg}");
            Err(1)
        }
    }
}

#[derive(Debug, Default)]
pub struct Summary {
    pub total: usize,
    pub ok: usize,
    pub planned: usize,
    pub aborted: usize,
    pub warnings: usize,
}

impl Summary {
    fn print(&self) {
        eprintln!(
            "tql reconcile: {} torrent(s): {} applied, {} planned, {} aborted, {} warnings",
            self.total, self.ok, self.planned, self.aborted, self.warnings
        );
    }
}

fn do_run(args: &Args) -> Result<Summary, String> {
    let (_, cfg) = config::load(args.config.as_deref()).map_err(|e| format!("config: {e}"))?;
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

    let torrents: Vec<TorrentInfo> = runtime.block_on(async {
        let client = qbit::Client::new(&qb.url).map_err(|e| format!("qbit client: {e}"))?;
        client
            .login(&qb.username, &password)
            .await
            .map_err(|e| format!("qbit login: {e}"))?;
        let query = TorrentsInfoQuery {
            hashes: args
                .torrent
                .as_ref()
                .map(|h| vec![h.clone()])
                .unwrap_or_default(),
            category: args.category.clone(),
            tag: None,
        };
        client
            .torrents_info(&query)
            .await
            .map_err(|e| format!("qbit torrents_info: {e}"))
    })?;

    let mut summary = Summary::default();
    let opts = post_process::ProcessOpts {
        dry_run: args.dry_run,
    };

    // Build the per-torrent task list. Torrents without a category can't
    // participate in tracker-qualified layout, so skip them up front (warn,
    // don't abort the run).
    enum Slot {
        Skip { hash: String, reason: String },
        Run(post_process::Args),
    }
    let plan: Vec<Slot> = torrents
        .iter()
        .map(|t| {
            let Some(category) = t.category.clone() else {
                return Slot::Skip {
                    hash: t.hash.clone(),
                    reason: "no category (tracker-qualified layout requires one)".into(),
                };
            };
            let content_path = if t.content_path.is_empty() {
                PathBuf::from(&t.save_path).join(&t.name)
            } else {
                PathBuf::from(&t.content_path)
            };
            Slot::Run(post_process::Args {
                hash: t.hash.clone(),
                name: t.name.clone(),
                category,
                tags: t.tags.join(","),
                content_path: content_path.to_string_lossy().into_owned(),
                save_path: t.save_path.clone(),
                size: t.size,
                config: args.config.clone(),
            })
        })
        .collect();

    let parallelism = cfg.reconcile.parallelism.max(1) as usize;
    let cfg_shared = Arc::new(cfg);
    let outcomes: Vec<(String, Option<post_process::Outcome>, Option<String>)> =
        runtime.block_on(async {
            let sem = Arc::new(tokio::sync::Semaphore::new(parallelism));
            let mut handles = Vec::with_capacity(plan.len());
            for slot in plan {
                match slot {
                    Slot::Skip { hash, reason } => {
                        // Resolved synchronously — record as a `None` outcome.
                        handles.push(tokio::spawn(async move { (hash, None, Some(reason)) }));
                    }
                    Slot::Run(pp_args) => {
                        let permit = sem.clone().acquire_owned().await.expect("semaphore");
                        let cfg_t = cfg_shared.clone();
                        handles.push(tokio::task::spawn_blocking(move || {
                            let _p = permit;
                            let hash = pp_args.hash.clone();
                            let out = post_process::process_with_cfg(&pp_args, &cfg_t, opts);
                            (hash, Some(out), None)
                        }));
                    }
                }
            }
            let mut out = Vec::with_capacity(handles.len());
            for h in handles {
                out.push(h.await.expect("reconcile task panicked"));
            }
            out
        });

    for (hash, outcome, skip_reason) in outcomes {
        summary.total += 1;
        if let Some(reason) = skip_reason {
            summary.warnings += 1;
            eprintln!("tql reconcile: skip {hash}: {reason}");
            continue;
        }
        match outcome.expect("either skip or outcome") {
            post_process::Outcome::Ok { warnings, .. } => {
                summary.ok += 1;
                summary.warnings += warnings.len();
                for w in &warnings {
                    eprintln!("tql reconcile: {hash}: warn: {w}");
                }
            }
            post_process::Outcome::Planned {
                hash: phash,
                adds,
                removes,
                warnings,
            } => {
                summary.planned += 1;
                summary.warnings += warnings.len();
                for a in &adds {
                    println!("{phash}: + {a}");
                }
                for r in &removes {
                    println!("{phash}: - {r}");
                }
                for w in &warnings {
                    eprintln!("tql reconcile: {phash}: warn: {w}");
                }
            }
            post_process::Outcome::Aborted(reason) => {
                summary.aborted += 1;
                eprintln!("tql reconcile: {hash}: aborted: {reason}");
            }
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut d = std::env::temp_dir();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            d.push(format!("tql-rec-{}-{}-{}", tag, std::process::id(), nanos));
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

    /// Minimal HTTP/1.1 mock: accepts connections in a loop, hands each
    /// raw request to `handler`, writes the response, closes. Lives until
    /// the returned `stop` flag flips and a connection wakes the accept.
    fn spawn_mock<F>(handler: F) -> (String, Arc<AtomicBool>, thread::JoinHandle<()>)
    where
        F: Fn(&str) -> String + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                if stop_t.load(Ordering::SeqCst) {
                    break;
                }
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let mut buf = [0u8; 8192];
                let mut acc = Vec::new();
                loop {
                    let n = match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    acc.extend_from_slice(&buf[..n]);
                    if let Some(idx) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&acc[..idx]).unwrap_or("");
                        let cl = headers
                            .lines()
                            .find_map(|l| {
                                let l = l.trim();
                                if let Some(rest) =
                                    l.to_ascii_lowercase().strip_prefix("content-length:")
                                {
                                    rest.trim().parse::<usize>().ok()
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        if acc.len() >= idx + 4 + cl {
                            break;
                        }
                    }
                }
                let req = String::from_utf8_lossy(&acc).to_string();
                let resp = handler(&req);
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        (url, stop, handle)
    }

    fn ok_text(body: &str, extra: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n{}\r\n{}",
            body.len(),
            extra,
            body
        )
    }
    fn ok_json(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn write_file(p: &Path, body: &[u8]) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(p).unwrap();
        f.write_all(body).unwrap();
    }

    fn write_config(root: &Path, lib: &Path, seed: &Path, qb_url: &str) -> PathBuf {
        let cfg = format!(
            r#"
[paths]
seed_root = "{seed}"
library_root = "{lib}"
trackers_root = "{root}/trackers"

[linking]
prefer = "hardlink"
windows_compat = false

[qbittorrent]
url = "{qb}"
username = "admin"
password_env = "TQL_TEST_QB_PASSWORD"
"#,
            seed = seed.display(),
            lib = lib.display(),
            root = root.display(),
            qb = qb_url,
        );
        let p = root.join("config.toml");
        fs::write(&p, cfg).unwrap();
        p
    }

    fn router(req: &str, content_path: &str) -> String {
        if req.starts_with("POST /api/v2/auth/login ") {
            return ok_text("Ok.", "Set-Cookie: SID=tok; Path=/; HttpOnly\r\n");
        }
        if req.starts_with("GET /api/v2/torrents/info") {
            let json = format!(
                r#"[{{"hash":"deadbeef","name":"Book","category":"tracker.tld","tags":"link:Cat/Sub","save_path":"/seed","content_path":"{cp}","size":42}}]"#,
                cp = content_path
            );
            return ok_json(&json);
        }
        // Default: 404
        let body = "not found";
        format!(
            "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    #[test]
    fn reconcile_applies_pipeline_per_torrent() {
        let d = TempDir::new("apply");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let content = seed.join("Book.epub");
        write_file(&content, b"epub");
        let cp = content.to_string_lossy().into_owned();
        let (qb_url, _stop, _h) = spawn_mock(move |req| router(req, &cp));
        let cfg = write_config(d.path(), &lib, &seed, &qb_url);

        std::env::set_var("TQL_TEST_QB_PASSWORD", "secret");
        let summary = do_run(&Args {
            dry_run: false,
            torrent: None,
            category: None,
            config: Some(cfg),
        })
        .expect("reconcile do_run");

        assert_eq!(summary.total, 1);
        assert_eq!(summary.ok, 1);
        assert_eq!(summary.aborted, 0);
        // Linked target exists with same inode as the seed file.
        let target = lib.join("tracker.tld/Cat/Sub/Book");
        let sm = fs::metadata(&content).unwrap();
        let tm = fs::metadata(&target).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(sm.ino(), tm.ino());
    }

    #[test]
    fn reconcile_dry_run_reports_plan_without_touching_fs() {
        let d = TempDir::new("dry");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let content = seed.join("Book.epub");
        write_file(&content, b"epub");
        let cp = content.to_string_lossy().into_owned();
        let (qb_url, _stop, _h) = spawn_mock(move |req| router(req, &cp));
        let cfg = write_config(d.path(), &lib, &seed, &qb_url);

        std::env::set_var("TQL_TEST_QB_PASSWORD", "secret");
        let summary = do_run(&Args {
            dry_run: true,
            torrent: None,
            category: None,
            config: Some(cfg),
        })
        .expect("dry-run do_run");

        assert_eq!(summary.total, 1);
        assert_eq!(summary.ok, 0);
        assert_eq!(summary.planned, 1);
        // No file written under library_root/tracker.tld.
        assert!(!lib.join("tracker.tld").exists(), "dry-run mutated fs");
        // Sidecar must not be written.
        let sc = crate::sidecar::sidecar_path(&lib, "deadbeef");
        assert!(!sc.exists(), "dry-run wrote a sidecar");
    }

    #[test]
    fn reconcile_runs_multiple_torrents_with_bounded_parallelism() {
        let d = TempDir::new("multi");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        // Three distinct seed files → three torrents.
        let files: Vec<PathBuf> = (0..3)
            .map(|i| {
                let p = seed.join(format!("Book{i}.epub"));
                write_file(&p, format!("epub{i}").as_bytes());
                p
            })
            .collect();
        let files_for_mock = files.clone();
        let (qb_url, _stop, _h) = spawn_mock(move |req| {
            if req.starts_with("POST /api/v2/auth/login ") {
                return ok_text("Ok.", "Set-Cookie: SID=tok; Path=/; HttpOnly\r\n");
            }
            if req.starts_with("GET /api/v2/torrents/info") {
                let entries: Vec<String> = files_for_mock
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        format!(
                            r#"{{"hash":"deadbeef{i:04x}","name":"Book{i}","category":"tracker.tld","tags":"link:Cat/Sub{i}","save_path":"/seed","content_path":"{cp}","size":42}}"#,
                            i = i,
                            cp = p.to_string_lossy()
                        )
                    })
                    .collect();
                let json = format!("[{}]", entries.join(","));
                return ok_json(&json);
            }
            let body = "not found";
            format!(
                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        });
        // Config with explicit parallelism=2 to force semaphore exercise.
        let cfg_body = format!(
            r#"
[paths]
seed_root = "{seed}"
library_root = "{lib}"
trackers_root = "{root}/trackers"

[linking]
prefer = "hardlink"
windows_compat = false

[qbittorrent]
url = "{qb}"
username = "admin"
password_env = "TQL_TEST_QB_PASSWORD"

[reconcile]
parallelism = 2
"#,
            seed = seed.display(),
            lib = lib.display(),
            root = d.path().display(),
            qb = qb_url,
        );
        let cfg_path = d.path().join("config.toml");
        fs::write(&cfg_path, cfg_body).unwrap();

        std::env::set_var("TQL_TEST_QB_PASSWORD", "secret");
        let summary = do_run(&Args {
            dry_run: false,
            torrent: None,
            category: None,
            config: Some(cfg_path),
        })
        .expect("reconcile do_run");

        assert_eq!(summary.total, 3);
        assert_eq!(summary.ok, 3);
        assert_eq!(summary.aborted, 0);
        // Each torrent linked to its own sub-tag.
        use std::os::unix::fs::MetadataExt;
        for (i, src) in files.iter().enumerate() {
            let target = lib.join(format!("tracker.tld/Cat/Sub{i}/Book{i}"));
            let sm = fs::metadata(src).unwrap();
            let tm = fs::metadata(&target).expect("linked target");
            assert_eq!(sm.ino(), tm.ino(), "torrent {i} not linked");
        }
    }

    #[test]
    fn reconcile_missing_qbittorrent_section_errors() {
        let d = TempDir::new("nocfg");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        // Config WITHOUT a [qbittorrent] section.
        let body = format!(
            r#"
[paths]
seed_root = "{seed}"
library_root = "{lib}"
trackers_root = "{root}/trackers"
"#,
            seed = seed.display(),
            lib = lib.display(),
            root = d.path().display(),
        );
        let cfg_path = d.path().join("config.toml");
        fs::write(&cfg_path, body).unwrap();

        let err = do_run(&Args {
            dry_run: false,
            torrent: None,
            category: None,
            config: Some(cfg_path),
        })
        .unwrap_err();
        assert!(err.contains("qbittorrent"), "got: {err}");
    }
}
