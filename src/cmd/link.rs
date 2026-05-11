//! Shared implementation for `tql link add` / `tql link remove`
//! (DESIGN.md §7).
//!
//! Both subcommands take an info hash + a relative library path
//! (without the `link:` prefix), validate it against §5 producer rules,
//! mutate the torrent's tags in qBittorrent, and then run the same
//! diff/apply pipeline as `tql post-process` against the (now updated)
//! torrent. The on-disk side effect (create/remove a link site under
//! `<library_root>/<category>/<rel>/<name>`) comes from that
//! post-process pass — this command is a thin orchestrator.

use std::path::PathBuf;

use crate::cmd::post_process;
use crate::config;
use crate::paths::{self, LINK_TAG_PREFIX};
use crate::qbit;
use crate::qbit::types::{TorrentInfo, TorrentsInfoQuery};

#[derive(Debug, Clone, Copy)]
pub enum Op {
    Add,
    Remove,
}

impl Op {
    fn label(self) -> &'static str {
        match self {
            Op::Add => "link add",
            Op::Remove => "link remove",
        }
    }
}

pub fn run(
    op: Op,
    hash: &str,
    path: &str,
    config_path: Option<&std::path::Path>,
) -> Result<(), u8> {
    let (_, cfg) = match config::load(config_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tql {}: config: {e}", op.label());
            return Err(1);
        }
    };

    // §5 — validate the tag string offline before touching qBittorrent.
    // We can't enforce the StartsWithCategory rule yet (we don't know the
    // canonical category until we've fetched the torrent), so we pass
    // `None` here and re-check after the fetch.
    let tag = format!("{LINK_TAG_PREFIX}{path}");
    if let Err(e) = paths::parse_link_tag(&tag, None) {
        eprintln!("tql {}: invalid path {path:?}: {e:?}", op.label());
        return Err(1);
    }

    let qb = match cfg.qbittorrent.as_ref() {
        Some(q) => q,
        None => {
            eprintln!("tql {}: config has no [qbittorrent] section", op.label());
            return Err(1);
        }
    };
    let password = match std::env::var(&qb.password_env) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("tql {}: env var {} is not set", op.label(), qb.password_env);
            return Err(1);
        }
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tql {}: tokio runtime: {e}", op.label());
            return Err(1);
        }
    };

    let url = qb.url.clone();
    let username = qb.username.clone();
    let info: TorrentInfo =
        match runtime.block_on(mutate_qbit(op, &url, &username, &password, hash, &tag)) {
            Ok(i) => i,
            Err(msg) => {
                eprintln!("tql {}: {msg}", op.label());
                return Err(1);
            }
        };

    // Re-validate now that we know the canonical category.
    if let Some(cat) = info.category.as_deref() {
        if let Err(e) = paths::parse_link_tag(&tag, Some(cat)) {
            eprintln!(
                "tql {}: path {path:?} not allowed for category {cat:?}: {e:?}",
                op.label()
            );
            return Err(1);
        }
    } else {
        eprintln!(
            "tql {}: torrent {hash} has no category (tracker-qualified layout requires one)",
            op.label()
        );
        return Err(1);
    }

    // Run the same pipeline post-process uses. The tag we mutated is now
    // reflected in `info.tags`; synthesize Args and re-use the shared
    // process_with_cfg core.
    let pp_args = build_pp_args(&info, config_path.map(PathBuf::from));
    match post_process::process_with_cfg(&pp_args, &cfg, post_process::ProcessOpts::default()) {
        post_process::Outcome::Ok { warnings, .. } => {
            for w in &warnings {
                eprintln!("tql {}: warn: {w}", op.label());
            }
            Ok(())
        }
        post_process::Outcome::Planned { .. } => Ok(()),
        post_process::Outcome::Aborted(reason) => {
            eprintln!("tql {}: aborted: {reason}", op.label());
            Err(1)
        }
    }
}

async fn mutate_qbit(
    op: Op,
    url: &str,
    username: &str,
    password: &str,
    hash: &str,
    tag: &str,
) -> Result<TorrentInfo, String> {
    let client = qbit::Client::new(url).map_err(|e| format!("qbit client: {e}"))?;
    client
        .login(username, password)
        .await
        .map_err(|e| format!("qbit login: {e}"))?;

    let hashes = vec![hash.to_string()];
    let tags = vec![tag.to_string()];
    match op {
        Op::Add => client
            .add_tags(&hashes, &tags)
            .await
            .map_err(|e| format!("qbit addTags: {e}"))?,
        Op::Remove => client
            .remove_tags(&hashes, &tags)
            .await
            .map_err(|e| format!("qbit removeTags: {e}"))?,
    }

    let mut infos = client
        .torrents_info(&TorrentsInfoQuery {
            hashes: hashes.clone(),
            ..Default::default()
        })
        .await
        .map_err(|e| format!("qbit torrents_info: {e}"))?;
    if infos.is_empty() {
        return Err(format!("no torrent with hash {hash}"));
    }
    Ok(infos.swap_remove(0))
}

fn build_pp_args(info: &TorrentInfo, config: Option<PathBuf>) -> post_process::Args {
    let content_path = if info.content_path.is_empty() {
        PathBuf::from(&info.save_path).join(&info.name)
    } else {
        PathBuf::from(&info.content_path)
    };
    post_process::Args {
        hash: info.hash.clone(),
        name: info.name.clone(),
        category: info.category.clone().unwrap_or_default(),
        tags: info.tags.join(","),
        content_path: content_path.to_string_lossy().into_owned(),
        save_path: info.save_path.clone(),
        size: info.size,
        config,
    }
}

/// Pure helper: compute the tag list after applying `op` to `current`.
/// Idempotent — adding a present tag or removing an absent one is a no-op.
#[cfg(test)]
pub fn apply_op(op: Op, current: &[String], tag: &str) -> Vec<String> {
    let mut out: Vec<String> = current.iter().filter(|t| *t != tag).cloned().collect();
    if matches!(op, Op::Add) {
        out.push(tag.to_string());
    }
    out
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

    #[test]
    fn apply_add_is_idempotent() {
        let cur = vec!["link:A".to_string(), "info:x".to_string()];
        let r = apply_op(Op::Add, &cur, "link:A");
        assert_eq!(r, vec!["info:x".to_string(), "link:A".to_string()]);
    }

    #[test]
    fn apply_add_appends_when_absent() {
        let cur = vec!["info:x".to_string()];
        let r = apply_op(Op::Add, &cur, "link:A");
        assert_eq!(r, vec!["info:x".to_string(), "link:A".to_string()]);
    }

    #[test]
    fn apply_remove_drops_tag() {
        let cur = vec!["link:A".to_string(), "info:x".to_string()];
        let r = apply_op(Op::Remove, &cur, "link:A");
        assert_eq!(r, vec!["info:x".to_string()]);
    }

    #[test]
    fn apply_remove_absent_is_noop() {
        let cur = vec!["info:x".to_string()];
        let r = apply_op(Op::Remove, &cur, "link:A");
        assert_eq!(r, vec!["info:x".to_string()]);
    }

    // ---------- end-to-end against a qBittorrent mock ----------

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut d = std::env::temp_dir();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            d.push(format!("tql-link-{}-{}-{}", tag, std::process::id(), nanos));
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

    fn write_config(root: &Path, lib: &Path, seed: &Path, qb_url: &str, pw_env: &str) -> PathBuf {
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
password_env = "{pw}"
"#,
            seed = seed.display(),
            lib = lib.display(),
            root = root.display(),
            qb = qb_url,
            pw = pw_env,
        );
        let p = root.join("config.toml");
        fs::write(&p, cfg).unwrap();
        p
    }

    #[test]
    fn link_add_end_to_end_creates_link_and_sidecar() {
        let d = TempDir::new("add");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let content = seed.join("Book.epub");
        write_file(&content, b"epub bytes");
        let cp = content.to_string_lossy().into_owned();

        // qBittorrent mock — log each request path so we can assert on order.
        let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log_h = log.clone();
        let seed_for_mock = seed.clone();
        let (qb_url, _stop, _h) = spawn_mock(move |req| {
            let first = req.lines().next().unwrap_or("").to_string();
            log_h.lock().unwrap().push(first.clone());
            if req.starts_with("POST /api/v2/auth/login ") {
                return ok_text("Ok.", "Set-Cookie: SID=tok; Path=/; HttpOnly\r\n");
            }
            if req.starts_with("POST /api/v2/torrents/addTags ") {
                // qBittorrent returns HTTP 200 with empty body on success.
                return ok_text("", "");
            }
            if req.starts_with("GET /api/v2/torrents/info") {
                // After addTags, the tag is reflected on the torrent.
                let json = format!(
                    r#"[{{"hash":"deadbeef","name":"Book","category":"tracker.tld","tags":"link:Cat/Sub","save_path":"{sp}","content_path":"{cp}","size":10}}]"#,
                    sp = seed_for_mock.to_string_lossy(),
                    cp = cp,
                );
                return ok_json(&json);
            }
            let body = "not found";
            format!(
                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        });

        let pw_env = format!("TQL_TEST_LINK_PW_{}", std::process::id());
        std::env::set_var(&pw_env, "secret");
        let cfg = write_config(d.path(), &lib, &seed, &qb_url, &pw_env);

        run(Op::Add, "deadbeef", "Cat/Sub", Some(&cfg)).expect("link add should succeed");

        // Mock saw: login, addTags, info (in that order).
        let entries = log.lock().unwrap().clone();
        assert!(
            entries
                .iter()
                .any(|l| l.starts_with("POST /api/v2/auth/login")),
            "no login: {entries:?}"
        );
        let add_idx = entries
            .iter()
            .position(|l| l.starts_with("POST /api/v2/torrents/addTags"))
            .expect("addTags missing");
        let info_idx = entries
            .iter()
            .position(|l| l.starts_with("GET /api/v2/torrents/info"))
            .expect("info missing");
        assert!(add_idx < info_idx, "addTags must precede info: {entries:?}");

        // Linked target exists with same inode as the seed file.
        let target = lib.join("tracker.tld/Cat/Sub/Book");
        let sm = fs::metadata(&content).unwrap();
        let tm = fs::metadata(&target).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(sm.ino(), tm.ino(), "hardlink inode mismatch");

        // Sidecar written.
        let sc_path = crate::sidecar::sidecar_path(&lib, "deadbeef");
        let sc = crate::sidecar::read(&sc_path).unwrap().unwrap();
        assert_eq!(sc.name, "Book");
        assert_eq!(sc.link_sites.len(), 1);
        assert_eq!(sc.link_sites[0].relative_path, "Cat/Sub");

        std::env::remove_var(&pw_env);
    }

    #[test]
    fn link_remove_end_to_end_unlinks_and_updates_sidecar() {
        let d = TempDir::new("rm");
        let seed = d.path().join("seed");
        let lib = d.path().join("lib");
        fs::create_dir_all(&seed).unwrap();
        fs::create_dir_all(&lib).unwrap();
        let content = seed.join("Book.epub");
        write_file(&content, b"epub bytes");
        let cp = content.to_string_lossy().into_owned();

        // Pre-create the link site (as if a prior `link add` had run) and a
        // sidecar reflecting that state — so post_process diff sees the
        // tag removal as "remove site Cat/Sub".
        let target_dir = lib.join("tracker.tld/Cat/Sub");
        fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("Book");
        fs::hard_link(&content, &target).unwrap();

        let sc = crate::sidecar::Sidecar {
            schema_version: crate::sidecar::SCHEMA_VERSION,
            info_hash_v1: "deadbeef".into(),
            info_hash_v2: None,
            name: "Book".into(),
            category: "tracker.tld".into(),
            content_path: cp.clone(),
            is_directory: false,
            size_bytes: 10,
            link_sites: vec![crate::sidecar::LinkSite {
                relative_path: "Cat/Sub".into(),
                resolved_path: target.to_string_lossy().into_owned(),
                created_at: "1970-01-01T00:00:00Z".into(),
                origin: crate::sidecar::Origin::PostProcess,
            }],
            last_applied_tags: vec!["link:Cat/Sub".into()],
            last_applied_at: None,
            warnings: vec![],
        };
        let sc_path = crate::sidecar::sidecar_path(&lib, "deadbeef");
        crate::sidecar::write(&sc_path, &sc).unwrap();

        // qBittorrent mock — after removeTags, info reflects empty tags.
        let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log_h = log.clone();
        let seed_for_mock = seed.clone();
        let (qb_url, _stop, _h) = spawn_mock(move |req| {
            let first = req.lines().next().unwrap_or("").to_string();
            log_h.lock().unwrap().push(first.clone());
            if req.starts_with("POST /api/v2/auth/login ") {
                return ok_text("Ok.", "Set-Cookie: SID=tok; Path=/; HttpOnly\r\n");
            }
            if req.starts_with("POST /api/v2/torrents/removeTags ") {
                return ok_text("", "");
            }
            if req.starts_with("GET /api/v2/torrents/info") {
                let json = format!(
                    r#"[{{"hash":"deadbeef","name":"Book","category":"tracker.tld","tags":"","save_path":"{sp}","content_path":"{cp}","size":10}}]"#,
                    sp = seed_for_mock.to_string_lossy(),
                    cp = cp,
                );
                return ok_json(&json);
            }
            let body = "not found";
            format!(
                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        });

        let pw_env = format!("TQL_TEST_LINK_RM_PW_{}", std::process::id());
        std::env::set_var(&pw_env, "secret");
        let cfg = write_config(d.path(), &lib, &seed, &qb_url, &pw_env);

        run(Op::Remove, "deadbeef", "Cat/Sub", Some(&cfg)).expect("link remove should succeed");

        let entries = log.lock().unwrap().clone();
        let rm_idx = entries
            .iter()
            .position(|l| l.starts_with("POST /api/v2/torrents/removeTags"))
            .expect("removeTags missing");
        let info_idx = entries
            .iter()
            .position(|l| l.starts_with("GET /api/v2/torrents/info"))
            .expect("info missing");
        assert!(
            rm_idx < info_idx,
            "removeTags must precede info: {entries:?}"
        );

        assert!(!target.exists(), "link target should be gone");
        // Newly-empty parents under <library>/<category>/ pruned up to the
        // category boundary (paths.rs / linking.rs semantics).
        assert!(
            !lib.join("tracker.tld/Cat").exists(),
            "empty parent dirs should be pruned"
        );

        let sc_after = crate::sidecar::read(&sc_path).unwrap().unwrap();
        assert!(
            sc_after.link_sites.is_empty(),
            "sidecar link_sites should be empty after remove: {:?}",
            sc_after.link_sites
        );

        std::env::remove_var(&pw_env);
    }
}
