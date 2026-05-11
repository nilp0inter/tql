//! Jellyfin partial-refresh backend.
//!
//! Single `POST <url>/Library/Media/Updated` with body
//! `{ "Updates": [{ "Path": "...", "UpdateType": "Modified" }, …] }`,
//! authenticated by an `X-Emby-Token` header. One request per refresh call
//! (Jellyfin batches across the array). 5 s timeout, no retry.

use std::path::PathBuf;
use std::time::Duration;

use crate::config::Jellyfin;

use super::TIMEOUT_SECS;

pub async fn refresh(cfg: &Jellyfin, api_key: &str, paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("media jellyfin: client build: {e}"))?;
    let updates: Vec<serde_json::Value> = paths
        .iter()
        .map(|p| {
            serde_json::json!({
                "Path": p.to_string_lossy(),
                "UpdateType": "Modified",
            })
        })
        .collect();
    let body = serde_json::json!({ "Updates": updates });
    let url = format!("{}/Library/Media/Updated", cfg.url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .header("X-Emby-Token", api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("media jellyfin: {e}"))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("media jellyfin: HTTP {status}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    fn spawn_mock(status: u16) -> (String, Arc<Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let captured = Arc::new(Mutex::new(String::new()));
        let cap_t = captured.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
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
                *cap_t.lock().unwrap() = String::from_utf8_lossy(&acc).to_string();
                let body = r#"{}"#;
                let phrase = if status == 200 { "OK" } else { "ERR" };
                let resp = format!(
                    "HTTP/1.1 {status} {phrase}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (url, captured)
    }

    #[tokio::test]
    async fn posts_batched_updates() {
        let (url, cap) = spawn_mock(200);
        let cfg = Jellyfin {
            url,
            api_key_env: "UNUSED".into(),
        };
        let paths = vec![PathBuf::from("/lib/a"), PathBuf::from("/lib/b")];
        refresh(&cfg, "KEY", &paths).await.expect("ok");
        let req = cap.lock().unwrap().clone();
        assert!(req.contains("POST /Library/Media/Updated"), "req: {req}");
        let lc = req.to_ascii_lowercase();
        assert!(lc.contains("x-emby-token: key"), "req: {req}");
        assert!(req.contains(r#""Path":"/lib/a""#));
        assert!(req.contains(r#""Path":"/lib/b""#));
        assert!(req.contains(r#""UpdateType":"Modified""#));
    }

    #[tokio::test]
    async fn empty_paths_is_noop() {
        let cfg = Jellyfin {
            url: "http://127.0.0.1:1".into(),
            api_key_env: "UNUSED".into(),
        };
        refresh(&cfg, "K", &[]).await.expect("ok");
    }

    #[tokio::test]
    async fn non_2xx_is_error() {
        let (url, _cap) = spawn_mock(500);
        let cfg = Jellyfin {
            url,
            api_key_env: "UNUSED".into(),
        };
        let err = refresh(&cfg, "K", &[PathBuf::from("/x")])
            .await
            .unwrap_err();
        assert!(err.contains("500"));
    }
}
