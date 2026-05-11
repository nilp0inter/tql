//! `tql notify-flush` — drain the notification spool and dispatch.
//!
//! Per DESIGN.md §15: invoked by a systemd timer, debounces (5 s),
//! batches (≤10 per Telegram message), atomically drains the spool, and
//! requeues any batch the backend couldn't accept so the next run retries.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use clap::Parser;

use crate::config::{self, Config, Telegram};
use crate::notify::{self, Event};
use crate::notify::telegram::{self, MAX_BATCH};

/// Default debounce window. The spool's mtime must be older than this for a
/// flush to proceed unless `--force` is passed.
const DEBOUNCE: Duration = Duration::from_secs(5);

#[derive(Parser, Debug)]
pub struct Args {
    /// Optional config-file override.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Print pending events as JSON instead of dispatching them.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip the 5 s debounce check (use when invoked manually).
    #[arg(long)]
    pub force: bool,
    /// Send at most this many events this run (default: unlimited).
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

pub fn run(args: Args) -> Result<(), u8> {
    let (_path, cfg) = match config::load(args.config.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tql notify-flush: {e}");
            return Err(2);
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("tql notify-flush: tokio runtime: {e}");
            return Err(2);
        }
    };
    rt.block_on(async {
        match flush(&cfg, &args, telegram::DEFAULT_BASE_URL).await {
            Outcome::Ok { sent, requeued } => {
                eprintln!("tql notify-flush: sent {sent}, requeued {requeued}");
                Ok(())
            }
            Outcome::Debounced => {
                eprintln!("tql notify-flush: debounced (spool mtime within 5 s)");
                Ok(())
            }
            Outcome::DryRun { pending } => {
                for ev in &pending {
                    println!("{}", serde_json::to_string(ev).unwrap());
                }
                Ok(())
            }
            Outcome::Error(msg) => {
                eprintln!("tql notify-flush: {msg}");
                Err(1)
            }
        }
    })
}

#[derive(Debug)]
pub enum Outcome {
    Ok { sent: usize, requeued: usize },
    Debounced,
    DryRun { pending: Vec<Event> },
    Error(String),
}

pub async fn flush(cfg: &Config, args: &Args, telegram_base_url: &str) -> Outcome {
    let spool = cfg
        .notify
        .spool_path
        .clone()
        .unwrap_or_else(|| notify::default_spool_path(&cfg.paths.library_root));

    if args.dry_run {
        return match notify::read_all(&spool) {
            Ok(events) => Outcome::DryRun { pending: events },
            Err(e) => Outcome::Error(format!("read spool: {e}")),
        };
    }

    if !args.force {
        if let Ok(meta) = fs::metadata(&spool) {
            if let Ok(modified) = meta.modified() {
                if SystemTime::now()
                    .duration_since(modified)
                    .map(|d| d < DEBOUNCE)
                    .unwrap_or(false)
                {
                    return Outcome::Debounced;
                }
            }
        }
    }

    let events = match notify::drain(&spool) {
        Ok(e) => e,
        Err(e) => return Outcome::Error(format!("drain spool: {e}")),
    };
    if events.is_empty() {
        let _ = notify::commit_drain(&spool);
        return Outcome::Ok { sent: 0, requeued: 0 };
    }

    let limit = args.limit.unwrap_or(events.len()).min(events.len());
    let (todo, defer) = events.split_at(limit);

    let mut sent = 0usize;
    let mut failed_idx: Option<usize> = None;
    for (i, chunk) in todo.chunks(MAX_BATCH).enumerate() {
        match dispatch_chunk(cfg, telegram_base_url, chunk).await {
            Ok(()) => sent += chunk.len(),
            Err(msg) => {
                eprintln!("tql notify-flush: batch {i} failed: {msg}");
                failed_idx = Some(i * MAX_BATCH);
                break;
            }
        }
    }

    let mut tail: Vec<Event> = Vec::new();
    if let Some(start) = failed_idx {
        tail.extend_from_slice(&todo[start..]);
    }
    tail.extend_from_slice(defer);

    let requeued = tail.len();
    if let Err(e) = notify::requeue(&spool, &tail) {
        return Outcome::Error(format!("requeue: {e}"));
    }
    Outcome::Ok { sent, requeued }
}

async fn dispatch_chunk(
    cfg: &Config,
    telegram_base_url: &str,
    events: &[Event],
) -> Result<(), String> {
    let backends = if cfg.notify.default.is_empty() {
        // No explicit list: pick whatever is configured.
        let mut v = Vec::new();
        if cfg.notify.telegram.is_some() {
            v.push("telegram".to_string());
        }
        v
    } else {
        cfg.notify.default.clone()
    };
    if backends.is_empty() {
        // No backend configured: log the events to stderr so the timer
        // operator at least sees them; treat as success.
        for ev in events {
            eprintln!("tql notify-flush: (no backend) {} [{}]", ev.name, ev.category);
        }
        return Ok(());
    }
    for backend in &backends {
        match backend.as_str() {
            "telegram" => dispatch_telegram(cfg, telegram_base_url, events).await?,
            other => return Err(format!("unknown backend: {other}")),
        }
    }
    Ok(())
}

async fn dispatch_telegram(
    cfg: &Config,
    base_url: &str,
    events: &[Event],
) -> Result<(), String> {
    let tg: &Telegram = cfg
        .notify
        .telegram
        .as_ref()
        .ok_or_else(|| "telegram backend selected but [notify.telegram] is missing".to_string())?;
    let token = std::env::var(&tg.bot_token_env)
        .map_err(|_| format!("env var {} is not set", tg.bot_token_env))?;
    telegram::send_batch(base_url, &token, &tg.chat_id, &tg.parse_mode, events)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Notify, Paths, Telegram};
    use crate::notify::{enqueue, Event as NEvent, EVENT_SCHEMA_VERSION};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut d = std::env::temp_dir();
            let n = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            d.push(format!("tql-nf-{tag}-{}-{n}", std::process::id()));
            fs::create_dir_all(&d).unwrap();
            Self(d)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn ev(hash: &str) -> NEvent {
        NEvent {
            schema_version: EVENT_SCHEMA_VERSION,
            ts: "2026-05-11T12:00:00Z".into(),
            info_hash_v1: hash.into(),
            name: format!("Item {hash}"),
            category: "tracker.tld".into(),
            link_sites_added: vec!["Comp/Author".into()],
            link_sites_removed: vec![],
            warnings: vec![],
        }
    }

    fn make_cfg(library_root: &std::path::Path, tg: Option<Telegram>) -> Config {
        Config {
            paths: Paths {
                seed_root: library_root.to_path_buf(),
                library_root: library_root.to_path_buf(),
                trackers_root: library_root.to_path_buf(),
            },
            linking: Default::default(),
            qbittorrent: None,
            notify: Notify {
                default: if tg.is_some() {
                    vec!["telegram".into()]
                } else {
                    vec![]
                },
                spool_path: None,
                telegram: tg,
            },
            media: Default::default(),
            reconcile: Default::default(),
            mcp: Default::default(),
            api: Default::default(),
            scripting: Default::default(),
            trackers: Default::default(),
        }
    }

    fn spawn_mock<F>(handler: F) -> (String, Arc<AtomicBool>, thread::JoinHandle<()>)
    where
        F: Fn(&str) -> String + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let h = thread::spawn(move || {
            for stream in listener.incoming() {
                if stop_t.load(Ordering::SeqCst) {
                    break;
                }
                let mut s = match stream {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let mut buf = [0u8; 8192];
                let mut acc = Vec::new();
                loop {
                    let n = match s.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    acc.extend_from_slice(&buf[..n]);
                    if let Some(idx) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&acc[..idx]).unwrap_or("");
                        let cl = headers
                            .lines()
                            .find_map(|l| {
                                let l = l.trim().to_ascii_lowercase();
                                l.strip_prefix("content-length:")
                                    .and_then(|r| r.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if acc.len() >= idx + 4 + cl {
                            break;
                        }
                    }
                }
                let req = String::from_utf8_lossy(&acc).to_string();
                let resp = handler(&req);
                let _ = s.write_all(resp.as_bytes());
                let _ = s.flush();
            }
        });
        (url, stop, h)
    }

    fn ok_resp(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn back_date_spool(spool: &std::path::Path) {
        // Move mtime back so the debounce check passes.
        let past = SystemTime::now() - Duration::from_secs(60);
        let times = std::fs::FileTimes::new().set_modified(past);
        let f = std::fs::OpenOptions::new().write(true).open(spool).unwrap();
        f.set_times(times).unwrap();
    }

    #[tokio::test]
    async fn empty_spool_is_a_noop() {
        let d = TempDir::new("empty");
        let cfg = make_cfg(&d.0, None);
        let args = Args { config: None, dry_run: false, force: true, limit: None };
        match flush(&cfg, &args, "http://unused").await {
            Outcome::Ok { sent, requeued } => {
                assert_eq!(sent, 0);
                assert_eq!(requeued, 0);
            }
            other => panic!("wrong outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn debounce_skips_recent_spool() {
        let d = TempDir::new("debounce");
        let cfg = make_cfg(&d.0, None);
        let spool = notify::default_spool_path(&d.0);
        enqueue(&spool, &ev("a")).unwrap();
        let args = Args { config: None, dry_run: false, force: false, limit: None };
        match flush(&cfg, &args, "http://unused").await {
            Outcome::Debounced => {}
            other => panic!("wrong outcome: {other:?}"),
        }
        // Event must still be there.
        assert_eq!(notify::read_all(&spool).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn dry_run_prints_without_draining() {
        let d = TempDir::new("dry");
        let cfg = make_cfg(&d.0, None);
        let spool = notify::default_spool_path(&d.0);
        enqueue(&spool, &ev("a")).unwrap();
        let args = Args { config: None, dry_run: true, force: true, limit: None };
        match flush(&cfg, &args, "http://unused").await {
            Outcome::DryRun { pending } => assert_eq!(pending.len(), 1),
            other => panic!("wrong outcome: {other:?}"),
        }
        // Spool still has the event.
        assert_eq!(notify::read_all(&spool).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn success_drains_and_sends() {
        let count = Arc::new(Mutex::new(0u32));
        let c = count.clone();
        let (base, _stop, _h) = spawn_mock(move |_req| {
            *c.lock().unwrap() += 1;
            ok_resp(r#"{"ok":true,"result":{}}"#)
        });
        let d = TempDir::new("ok");
        let tg = Telegram {
            bot_token_env: "TQL_TEST_TG_TOKEN".into(),
            chat_id: "-100".into(),
            parse_mode: "HTML".into(),
        };
        std::env::set_var("TQL_TEST_TG_TOKEN", "x");
        let cfg = make_cfg(&d.0, Some(tg));
        let spool = notify::default_spool_path(&d.0);
        for i in 0..3 {
            enqueue(&spool, &ev(&format!("h{i}"))).unwrap();
        }
        back_date_spool(&spool);
        let args = Args { config: None, dry_run: false, force: true, limit: None };
        match flush(&cfg, &args, &base).await {
            Outcome::Ok { sent, requeued } => {
                assert_eq!(sent, 3);
                assert_eq!(requeued, 0);
            }
            other => panic!("wrong outcome: {other:?}"),
        }
        assert_eq!(*count.lock().unwrap(), 1, "one batch");
        assert!(!spool.exists());
        assert!(!notify::flushing_path(&spool).exists());
    }

    #[tokio::test]
    async fn failure_requeues_unsent_tail() {
        // Mock fails every call.
        let (base, _stop, _h) = spawn_mock(|_| ok_resp(r#"{"ok":false,"description":"nope"}"#));
        let d = TempDir::new("fail");
        let tg = Telegram {
            bot_token_env: "TQL_TEST_TG_TOKEN".into(),
            chat_id: "-100".into(),
            parse_mode: "HTML".into(),
        };
        std::env::set_var("TQL_TEST_TG_TOKEN", "x");
        let cfg = make_cfg(&d.0, Some(tg));
        let spool = notify::default_spool_path(&d.0);
        for i in 0..3 {
            enqueue(&spool, &ev(&format!("h{i}"))).unwrap();
        }
        back_date_spool(&spool);
        let args = Args { config: None, dry_run: false, force: true, limit: None };
        match flush(&cfg, &args, &base).await {
            Outcome::Ok { sent, requeued } => {
                assert_eq!(sent, 0);
                assert_eq!(requeued, 3);
            }
            other => panic!("wrong outcome: {other:?}"),
        }
        // Spool now holds the requeued events.
        assert_eq!(notify::read_all(&spool).unwrap().len(), 3);
    }

    #[tokio::test]
    async fn batches_respect_max_batch() {
        let calls = Arc::new(Mutex::new(0u32));
        let c = calls.clone();
        let (base, _stop, _h) = spawn_mock(move |_| {
            *c.lock().unwrap() += 1;
            ok_resp(r#"{"ok":true}"#)
        });
        let d = TempDir::new("batch");
        let tg = Telegram {
            bot_token_env: "TQL_TEST_TG_TOKEN".into(),
            chat_id: "-100".into(),
            parse_mode: "HTML".into(),
        };
        std::env::set_var("TQL_TEST_TG_TOKEN", "x");
        let cfg = make_cfg(&d.0, Some(tg));
        let spool = notify::default_spool_path(&d.0);
        for i in 0..23 {
            enqueue(&spool, &ev(&format!("h{i}"))).unwrap();
        }
        back_date_spool(&spool);
        let args = Args { config: None, dry_run: false, force: true, limit: None };
        let _ = flush(&cfg, &args, &base).await;
        // ceil(23 / 10) = 3 batches.
        assert_eq!(*calls.lock().unwrap(), 3);
    }
}
