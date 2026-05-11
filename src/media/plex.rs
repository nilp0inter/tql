//! Plex partial-refresh backend.
//!
//! For each (section_id, path) pair, issue
//! `GET <url>/library/sections/<id>/refresh?path=<path>&X-Plex-Token=<token>`.
//! Plex ignores paths that aren't under that section, so fanning out across
//! every configured section is the simplest correct strategy. Best-effort:
//! every transport/HTTP error becomes a warning. Per-request 5 s timeout,
//! no retry (per DESIGN.md §15).

use std::path::PathBuf;
use std::time::Duration;

use crate::config::Plex;

use super::TIMEOUT_SECS;

/// Refresh every (section, path) pair. Returns the collected warnings;
/// `Ok(())` means every request returned 2xx.
pub async fn refresh(cfg: &Plex, token: &str, paths: &[PathBuf]) -> Result<(), Vec<String>> {
    if cfg.section_ids.is_empty() || paths.is_empty() {
        return Ok(());
    }
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Err(vec![format!("media plex: client build: {e}")]),
    };
    let base = cfg.url.trim_end_matches('/').to_string();
    let mut warnings = Vec::new();
    for section in &cfg.section_ids {
        for p in paths {
            let url = format!("{base}/library/sections/{section}/refresh");
            let req = client
                .get(&url)
                .query(&[("path", p.to_string_lossy().as_ref()), ("X-Plex-Token", token)])
                .header("Accept", "application/json");
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if !(200..300).contains(&status) {
                        warnings.push(format!(
                            "media plex: section {section} path {}: HTTP {status}",
                            p.display()
                        ));
                    }
                }
                Err(e) => warnings.push(format!(
                    "media plex: section {section} path {}: {e}",
                    p.display()
                )),
            }
        }
    }
    if warnings.is_empty() {
        Ok(())
    } else {
        Err(warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn spawn_mock<F>(handler: F) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>)
    where
        F: Fn(&str) -> String + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let hits = Arc::new(AtomicUsize::new(0));
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let hits_t = hits.clone();
        let cap_t = captured.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let mut buf = [0u8; 4096];
                let n = match stream.read(&mut buf) {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                hits_t.fetch_add(1, Ordering::SeqCst);
                cap_t.lock().unwrap().push(req.clone());
                let resp = handler(&req);
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (url, hits, captured)
    }

    fn ok_response() -> String {
        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
    }

    #[tokio::test]
    async fn fans_out_per_section_and_path() {
        let (url, hits, cap) = spawn_mock(|_| ok_response());
        let cfg = Plex {
            url,
            token_env: "UNUSED".into(),
            section_ids: vec![1, 2],
        };
        let paths = vec![PathBuf::from("/lib/a"), PathBuf::from("/lib/b")];
        refresh(&cfg, "TOKEN", &paths).await.expect("ok");
        // 2 sections × 2 paths.
        assert_eq!(hits.load(Ordering::SeqCst), 4);
        let reqs = cap.lock().unwrap().clone();
        // Each request must carry the path and token.
        assert!(reqs.iter().any(|r| r.contains("path=%2Flib%2Fa") || r.contains("path=/lib/a")));
        assert!(reqs.iter().all(|r| r.contains("X-Plex-Token=TOKEN")));
        // Targets the section refresh endpoint.
        assert!(reqs.iter().any(|r| r.contains("/library/sections/1/refresh")));
        assert!(reqs.iter().any(|r| r.contains("/library/sections/2/refresh")));
    }

    #[tokio::test]
    async fn non_2xx_becomes_warning() {
        let (url, _hits, _cap) = spawn_mock(|_| {
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_string()
        });
        let cfg = Plex {
            url,
            token_env: "UNUSED".into(),
            section_ids: vec![1],
        };
        let err = refresh(&cfg, "T", &[PathBuf::from("/x")]).await.unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].contains("503"));
    }

    #[tokio::test]
    async fn empty_sections_is_noop() {
        let cfg = Plex {
            url: "http://127.0.0.1:1".into(),
            token_env: "UNUSED".into(),
            section_ids: vec![],
        };
        refresh(&cfg, "T", &[PathBuf::from("/x")]).await.expect("ok");
    }
}
