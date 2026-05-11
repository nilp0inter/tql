//! Per-tracker .torrent fetch with credentials (DESIGN.md §8).
//!
//! When a transport receives a `source = url` and the tracker has
//! `[trackers.<name>]` credentials configured, the Rust core fetches the
//! .torrent itself with `reqwest`, attaching the configured cookie or
//! `Authorization` header from environment, and uploads the bytes to
//! qBittorrent as a file (so the qBittorrent process never sees the
//! credential and never has to be reachable from the tracker).
//!
//! The script never sees credentials — they live only in this module.

use std::time::Duration;

use crate::config::TrackerCreds;

const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum FetchError {
    InvalidUrl(String),
    MissingEnv(String),
    Http(reqwest::Error),
    Status { status: u16, body: String },
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::InvalidUrl(u) => write!(f, "invalid torrent fetch url: {u}"),
            FetchError::MissingEnv(name) => write!(f, "env var {name} is not set"),
            FetchError::Http(e) => write!(f, "torrent fetch http error: {e}"),
            FetchError::Status { status, body } => {
                let preview: String = body.chars().take(200).collect();
                write!(f, "torrent fetch returned HTTP {status}: {preview}")
            }
        }
    }
}

impl std::error::Error for FetchError {}

/// Returns true when `creds` declares anything that would change the request
/// (cookie or Authorization header). Otherwise we leave the URL as a passthrough
/// for qBittorrent itself to fetch.
pub fn has_creds(creds: &TrackerCreds) -> bool {
    creds.cookie_env.is_some() || creds.auth_header_env.is_some()
}

/// Fetch the .torrent at `url` with the credentials declared in `creds`.
///
/// `cookie_env` produces `Cookie: <value>`; `auth_header_env` produces
/// `Authorization: <value>` (the value is taken verbatim, so it should
/// already include any `Bearer ` / `Token ` prefix the tracker expects).
pub async fn fetch_torrent_with_creds(
    url: &str,
    creds: &TrackerCreds,
) -> Result<Vec<u8>, FetchError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| FetchError::InvalidUrl(url.to_string()))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(FetchError::InvalidUrl(url.to_string()));
    }

    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(FetchError::Http)?;

    let mut req = client.get(parsed);
    if let Some(env) = &creds.cookie_env {
        let value = std::env::var(env).map_err(|_| FetchError::MissingEnv(env.clone()))?;
        req = req.header(reqwest::header::COOKIE, value);
    }
    if let Some(env) = &creds.auth_header_env {
        let value = std::env::var(env).map_err(|_| FetchError::MissingEnv(env.clone()))?;
        req = req.header(reqwest::header::AUTHORIZATION, value);
    }

    let resp = req.send().await.map_err(FetchError::Http)?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(FetchError::Status {
            status: status.as_u16(),
            body,
        });
    }
    let bytes = resp.bytes().await.map_err(FetchError::Http)?;
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;

    fn spawn_mock<F>(handler: F) -> (String, Arc<AtomicBool>, thread::JoinHandle<()>)
    where
        F: Fn(&str) -> Vec<u8> + Send + 'static,
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
                let Ok(mut stream) = stream else {
                    continue;
                };
                let mut buf = [0u8; 4096];
                let mut acc = Vec::new();
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => acc.extend_from_slice(&buf[..n]),
                    }
                    if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&acc).to_string();
                let resp = handler(&req);
                let _ = stream.write_all(&resp);
                let _ = stream.flush();
            }
        });
        (url, stop, handle)
    }

    fn ok_bytes(body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/x-bittorrent\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    fn unique_env(name: &str) -> String {
        format!("{}_{}_{}", name, std::process::id(), line!())
    }

    #[test]
    fn has_creds_detects_cookie_or_header() {
        assert!(!has_creds(&TrackerCreds::default()));
        assert!(has_creds(&TrackerCreds {
            cookie_env: Some("X".into()),
            auth_header_env: None,
        }));
        assert!(has_creds(&TrackerCreds {
            cookie_env: None,
            auth_header_env: Some("X".into()),
        }));
    }

    #[tokio::test]
    async fn fetches_with_cookie_header_from_env() {
        let env_name = unique_env("TQL_TEST_COOKIE");
        std::env::set_var(&env_name, "mam_id=secret123");
        let (base, _stop, _h) = spawn_mock(|req| {
            assert!(
                req.lines()
                    .any(|l| l.eq_ignore_ascii_case("cookie: mam_id=secret123")),
                "expected Cookie header in request:\n{req}"
            );
            ok_bytes(b"d8:announce20:http://tracker/annce")
        });
        let creds = TrackerCreds {
            cookie_env: Some(env_name.clone()),
            auth_header_env: None,
        };
        let bytes = fetch_torrent_with_creds(&format!("{base}/torrents.php?id=42"), &creds)
            .await
            .unwrap();
        assert!(bytes.starts_with(b"d8:announce"));
        std::env::remove_var(&env_name);
    }

    #[tokio::test]
    async fn fetches_with_authorization_header_from_env() {
        let env_name = unique_env("TQL_TEST_AUTH");
        std::env::set_var(&env_name, "Bearer abc.def.ghi");
        let (base, _stop, _h) = spawn_mock(|req| {
            assert!(
                req.lines()
                    .any(|l| l.eq_ignore_ascii_case("authorization: Bearer abc.def.ghi")),
                "expected Authorization header in request:\n{req}"
            );
            ok_bytes(b"d4:test4:datae")
        });
        let creds = TrackerCreds {
            cookie_env: None,
            auth_header_env: Some(env_name.clone()),
        };
        let bytes = fetch_torrent_with_creds(&format!("{base}/dl"), &creds)
            .await
            .unwrap();
        assert_eq!(bytes, b"d4:test4:datae");
        std::env::remove_var(&env_name);
    }

    #[tokio::test]
    async fn missing_env_var_is_an_error() {
        let creds = TrackerCreds {
            cookie_env: Some("TQL_TEST_DEFINITELY_UNSET_XXYYZZ".into()),
            auth_header_env: None,
        };
        let err = fetch_torrent_with_creds("http://127.0.0.1:1/x", &creds)
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::MissingEnv(_)));
    }

    #[tokio::test]
    async fn non_2xx_surfaces_status_and_body() {
        let (base, _stop, _h) = spawn_mock(|_| {
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 12\r\nConnection: close\r\n\r\nnot allowed!"
                .to_vec()
        });
        let err = fetch_torrent_with_creds(&format!("{base}/dl"), &TrackerCreds::default())
            .await
            .unwrap_err();
        match err {
            FetchError::Status { status, body } => {
                assert_eq!(status, 403);
                assert!(body.contains("not allowed"));
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_non_http_url() {
        let err = fetch_torrent_with_creds("magnet:?xt=urn:btih:abc", &TrackerCreds::default())
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::InvalidUrl(_)));
    }
}
