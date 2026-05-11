//! qBittorrent WebUI client.
//!
//! Per DESIGN.md §6. Async client built on `reqwest` with a cookie store.
//! Leg 6a covers `login` only; `add_torrent` / `torrents_info` arrive in later
//! sublegs.
//!
//! qBittorrent's WebUI is a stateful, cookie-authenticated HTTP API. Calling
//! `/api/v2/auth/login` with form-urlencoded `username=&password=` returns
//! `Ok.` as the body and sets a `SID` session cookie; subsequent calls must
//! carry that cookie. A wrong credential pair returns `Fails.` with HTTP 200,
//! so we have to inspect the body, not just the status code.

#![allow(dead_code)]

pub mod types;

use std::sync::Arc;
use std::time::Duration;

use reqwest::multipart;
use reqwest::Url;

use self::types::{AddTorrentParams, TorrentInfo, TorrentSource, TorrentsInfoQuery};

#[derive(Debug)]
pub enum Error {
    InvalidBaseUrl(String),
    Http(reqwest::Error),
    Banned,
    BadCredentials,
    UnexpectedBody(String),
    /// `/api/v2/torrents/add` returned a non-2xx status or `Fails.` body.
    AddFailed {
        status: u16,
        body: String,
    },
    /// `add_torrent` was called with neither file bytes nor a url.
    NothingToAdd,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidBaseUrl(s) => write!(f, "invalid qbittorrent base url: {s}"),
            Error::Http(e) => write!(f, "qbittorrent http error: {e}"),
            Error::Banned => write!(f, "qbittorrent: client temporarily banned (HTTP 403)"),
            Error::BadCredentials => {
                write!(f, "qbittorrent: bad credentials (login returned Fails.)")
            }
            Error::UnexpectedBody(b) => write!(f, "qbittorrent: unexpected login body: {b:?}"),
            Error::AddFailed { status, body } => {
                write!(
                    f,
                    "qbittorrent: add torrent failed (HTTP {status}): {body:?}"
                )
            }
            Error::NothingToAdd => write!(f, "qbittorrent: add_torrent called without file or url"),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

/// A logged-in (or not-yet-logged-in) qBittorrent WebUI client.
///
/// Cloning is cheap; the underlying `reqwest::Client` and cookie jar are
/// `Arc`-shared.
#[derive(Clone)]
pub struct Client {
    base: Url,
    http: reqwest::Client,
    _jar: Arc<reqwest::cookie::Jar>,
}

impl Client {
    /// Build a client for the given WebUI base URL. The URL must include a
    /// scheme and host (e.g. `http://127.0.0.1:8080`). Trailing slashes are
    /// allowed.
    pub fn new(base_url: &str) -> Result<Self, Error> {
        let mut base =
            Url::parse(base_url).map_err(|e| Error::InvalidBaseUrl(format!("{base_url}: {e}")))?;
        // Normalize so `join` works predictably: ensure trailing slash.
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        let jar = Arc::new(reqwest::cookie::Jar::default());
        let http = reqwest::Client::builder()
            .cookie_provider(jar.clone())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(Error::Http)?;
        Ok(Self {
            base,
            http,
            _jar: jar,
        })
    }

    /// POST `/api/v2/auth/login` with `username` and `password`. On success,
    /// the cookie jar now carries the `SID` session cookie and subsequent
    /// requests through this `Client` are authenticated.
    ///
    /// Returns `Err(BadCredentials)` when the server answers `Fails.`,
    /// `Err(Banned)` on HTTP 403 (qBittorrent's brute-force lockout), and
    /// `Err(UnexpectedBody(_))` if the body is neither `Ok.` nor `Fails.`.
    pub async fn login(&self, username: &str, password: &str) -> Result<(), Error> {
        let url = self
            .base
            .join("api/v2/auth/login")
            .map_err(|e| Error::InvalidBaseUrl(e.to_string()))?;
        let resp = self
            .http
            .post(url)
            // qBittorrent rejects login requests lacking a Referer matching the
            // configured WebUI host. Set it to the base URL to be safe.
            .header(reqwest::header::REFERER, self.base.as_str())
            .form(&[("username", username), ("password", password)])
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::Banned);
        }
        let body = resp.error_for_status()?.text().await?;
        match body.trim() {
            "Ok." => Ok(()),
            "Fails." => Err(Error::BadCredentials),
            other => Err(Error::UnexpectedBody(other.to_string())),
        }
    }

    /// POST `/api/v2/torrents/add` with the given `source` and parameters.
    ///
    /// On success qBittorrent returns HTTP 200 with body `Ok.`. A 200 with
    /// `Fails.` (most often: malformed `.torrent`, duplicate hash with
    /// `dupe` disabled, or unsupported url) is surfaced as `AddFailed`.
    /// HTTP 415 (no torrent attached) is also surfaced as `AddFailed`.
    pub async fn add_torrent(
        &self,
        source: TorrentSource,
        params: &AddTorrentParams,
    ) -> Result<(), Error> {
        let url = self
            .base
            .join("api/v2/torrents/add")
            .map_err(|e| Error::InvalidBaseUrl(e.to_string()))?;

        let mut form = multipart::Form::new();
        match source {
            TorrentSource::File { filename, bytes } => {
                let part = multipart::Part::bytes(bytes)
                    .file_name(filename)
                    .mime_str("application/x-bittorrent")
                    .map_err(Error::Http)?;
                form = form.part("torrents", part);
            }
            TorrentSource::Url(u) => {
                if u.is_empty() {
                    return Err(Error::NothingToAdd);
                }
                form = form.text("urls", u);
            }
        }
        if let Some(cat) = &params.category {
            form = form.text("category", cat.clone());
        }
        if !params.tags.is_empty() {
            form = form.text("tags", params.tags.join(","));
        }
        if let Some(p) = params.paused {
            form = form.text("paused", if p { "true" } else { "false" });
        }
        if let Some(a) = params.auto_tmm {
            form = form.text("autoTMM", if a { "true" } else { "false" });
        }
        if let Some(sp) = &params.savepath {
            form = form.text("savepath", sp.clone());
        }

        let resp = self
            .http
            .post(url)
            .header(reqwest::header::REFERER, self.base.as_str())
            .multipart(form)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::AddFailed {
                status: status.as_u16(),
                body,
            });
        }
        match body.trim() {
            "Ok." => Ok(()),
            // Anything else with HTTP 200 — most commonly "Fails." — is a
            // semantic failure.
            other => Err(Error::AddFailed {
                status: status.as_u16(),
                body: other.to_string(),
            }),
        }
    }

    /// GET `/api/v2/torrents/info` with optional filters. Returns the parsed
    /// list of `TorrentInfo` entries. An empty list is a normal result.
    ///
    /// Non-2xx responses are surfaced as `AddFailed` (shared error variant
    /// for "qBittorrent said no") with the response body for diagnostics.
    pub async fn torrents_info(
        &self,
        query: &TorrentsInfoQuery,
    ) -> Result<Vec<TorrentInfo>, Error> {
        let url = self
            .base
            .join("api/v2/torrents/info")
            .map_err(|e| Error::InvalidBaseUrl(e.to_string()))?;
        let mut req = self
            .http
            .get(url)
            .header(reqwest::header::REFERER, self.base.as_str());
        let mut params: Vec<(&str, String)> = Vec::new();
        if !query.hashes.is_empty() {
            params.push(("hashes", query.hashes.join("|")));
        }
        if let Some(c) = &query.category {
            params.push(("category", c.clone()));
        }
        if let Some(t) = &query.tag {
            params.push(("tag", t.clone()));
        }
        if !params.is_empty() {
            req = req.query(&params);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::AddFailed {
                status: status.as_u16(),
                body,
            });
        }
        let infos = resp.json::<Vec<TorrentInfo>>().await?;
        Ok(infos)
    }

    /// The base URL the client was constructed with (with a guaranteed
    /// trailing slash). Mostly useful for log messages.
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// POST `/api/v2/torrents/addTags` with `hashes=<a|b>&tags=<csv>`.
    /// Adds the given tags to every listed torrent. qBittorrent returns
    /// HTTP 200 with an empty body on success.
    pub async fn add_tags(&self, hashes: &[String], tags: &[String]) -> Result<(), Error> {
        self.set_tags("api/v2/torrents/addTags", hashes, tags).await
    }

    /// POST `/api/v2/torrents/removeTags` with `hashes=<a|b>&tags=<csv>`.
    pub async fn remove_tags(&self, hashes: &[String], tags: &[String]) -> Result<(), Error> {
        self.set_tags("api/v2/torrents/removeTags", hashes, tags)
            .await
    }

    async fn set_tags(&self, path: &str, hashes: &[String], tags: &[String]) -> Result<(), Error> {
        let url = self
            .base
            .join(path)
            .map_err(|e| Error::InvalidBaseUrl(e.to_string()))?;
        let resp = self
            .http
            .post(url)
            .header(reqwest::header::REFERER, self.base.as_str())
            .form(&[("hashes", hashes.join("|")), ("tags", tags.join(","))])
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::AddFailed {
                status: status.as_u16(),
                body,
            });
        }
        Ok(())
    }

    /// GET `/api/v2/app/version`. Used by `tql doctor` as a post-login
    /// reachability probe. Returns the raw version string (e.g. `"v4.6.4"`).
    pub async fn app_version(&self) -> Result<String, Error> {
        let url = self
            .base
            .join("api/v2/app/version")
            .map_err(|e| Error::InvalidBaseUrl(e.to_string()))?;
        let resp = self
            .http
            .get(url)
            .header(reqwest::header::REFERER, self.base.as_str())
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::AddFailed {
                status: status.as_u16(),
                body,
            });
        }
        Ok(body.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    /// Minimal HTTP/1.1 mock: accepts one connection, reads request headers
    /// and body, hands them to `handler`, writes the returned response, then
    /// closes. Returns `(base_url, stop_flag, join_handle)`. The accept loop
    /// runs until `stop_flag` flips or the listener is dropped.
    fn spawn_mock<F>(handler: F) -> (String, Arc<AtomicBool>, thread::JoinHandle<()>)
    where
        F: Fn(&str) -> String + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(false).unwrap();
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
                // Read until we have the whole request: simple heuristic —
                // read headers, then `Content-Length` bytes of body.
                loop {
                    let n = match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    acc.extend_from_slice(&buf[..n]);
                    if let Some(idx) = find_header_end(&acc) {
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

    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    fn ok_response(body: &str, extra_headers: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n{}\r\n{}",
            body.len(),
            extra_headers,
            body
        )
    }

    #[tokio::test]
    async fn login_success_sets_cookie() {
        let (base, _stop, _h) = spawn_mock(|req| {
            assert!(
                req.starts_with("POST /api/v2/auth/login "),
                "request: {req}"
            );
            assert!(req.contains("username=admin"));
            assert!(req.contains("password=secret"));
            ok_response("Ok.", "Set-Cookie: SID=abc123; Path=/; HttpOnly\r\n")
        });
        let client = Client::new(&base).unwrap();
        client
            .login("admin", "secret")
            .await
            .expect("login should succeed");
    }

    #[tokio::test]
    async fn login_bad_credentials() {
        let (base, _stop, _h) = spawn_mock(|_| ok_response("Fails.", ""));
        let client = Client::new(&base).unwrap();
        let err = client.login("admin", "wrong").await.unwrap_err();
        assert!(matches!(err, Error::BadCredentials), "got {err:?}");
    }

    #[tokio::test]
    async fn login_banned() {
        let (base, _stop, _h) = spawn_mock(|_| {
            let body = "Forbidden";
            format!(
                "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        });
        let client = Client::new(&base).unwrap();
        let err = client.login("admin", "secret").await.unwrap_err();
        assert!(matches!(err, Error::Banned), "got {err:?}");
    }

    #[tokio::test]
    async fn login_unexpected_body() {
        let (base, _stop, _h) = spawn_mock(|_| ok_response("???", ""));
        let client = Client::new(&base).unwrap();
        let err = client.login("admin", "secret").await.unwrap_err();
        assert!(matches!(err, Error::UnexpectedBody(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn add_torrent_file_success() {
        let (base, _stop, _h) = spawn_mock(|req| {
            assert!(
                req.starts_with("POST /api/v2/torrents/add "),
                "request: {req}"
            );
            assert!(
                req.contains("multipart/form-data"),
                "expected multipart, got: {req}"
            );
            // file part
            assert!(
                req.contains("name=\"torrents\""),
                "missing torrents part: {req}"
            );
            assert!(
                req.contains("filename=\"foo.torrent\""),
                "missing filename: {req}"
            );
            assert!(
                req.contains("application/x-bittorrent"),
                "missing mime: {req}"
            );
            assert!(req.contains("FAKE_TORRENT_BYTES"), "missing payload: {req}");
            // metadata parts
            assert!(req.contains("name=\"category\""), "missing category");
            assert!(req.contains("movies"), "missing category value");
            assert!(req.contains("name=\"tags\""), "missing tags");
            assert!(
                req.contains("link/movies/Foo,info/year/2020"),
                "missing tag csv"
            );
            assert!(req.contains("name=\"paused\""));
            assert!(req.contains("name=\"autoTMM\""));
            ok_response("Ok.", "")
        });
        let client = Client::new(&base).unwrap();
        let src = TorrentSource::File {
            filename: "foo.torrent".into(),
            bytes: b"FAKE_TORRENT_BYTES".to_vec(),
        };
        let params = AddTorrentParams {
            category: Some("movies".into()),
            tags: vec!["link/movies/Foo".into(), "info/year/2020".into()],
            paused: Some(false),
            auto_tmm: Some(true),
            savepath: None,
        };
        client
            .add_torrent(src, &params)
            .await
            .expect("add should succeed");
    }

    #[tokio::test]
    async fn add_torrent_url_success() {
        let (base, _stop, _h) = spawn_mock(|req| {
            assert!(req.contains("name=\"urls\""), "missing urls part: {req}");
            assert!(
                req.contains("magnet:?xt=urn:btih:DEADBEEF"),
                "missing magnet: {req}"
            );
            assert!(
                !req.contains("name=\"torrents\""),
                "should not have file part: {req}"
            );
            ok_response("Ok.", "")
        });
        let client = Client::new(&base).unwrap();
        let src = TorrentSource::Url("magnet:?xt=urn:btih:DEADBEEF".into());
        client
            .add_torrent(src, &AddTorrentParams::default())
            .await
            .expect("ok");
    }

    #[tokio::test]
    async fn add_torrent_fails_body() {
        let (base, _stop, _h) = spawn_mock(|_| ok_response("Fails.", ""));
        let client = Client::new(&base).unwrap();
        let src = TorrentSource::File {
            filename: "x.torrent".into(),
            bytes: vec![0u8; 4],
        };
        let err = client
            .add_torrent(src, &AddTorrentParams::default())
            .await
            .unwrap_err();
        match err {
            Error::AddFailed { status, body } => {
                assert_eq!(status, 200);
                assert_eq!(body, "Fails.");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn add_torrent_http_415() {
        let (base, _stop, _h) = spawn_mock(|_| {
            let body = "Torrent is not valid";
            format!(
                "HTTP/1.1 415 Unsupported Media Type\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        });
        let client = Client::new(&base).unwrap();
        let src = TorrentSource::Url("not a magnet".into());
        let err = client
            .add_torrent(src, &AddTorrentParams::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::AddFailed { status: 415, .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn add_torrent_empty_url_rejected() {
        // No mock needed — should fail before sending.
        let client = Client::new("http://127.0.0.1:1").unwrap();
        let err = client
            .add_torrent(
                TorrentSource::Url(String::new()),
                &AddTorrentParams::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NothingToAdd), "got {err:?}");
    }

    fn json_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    #[tokio::test]
    async fn torrents_info_parses_list_and_normalizes_fields() {
        let json = r#"[
            {"hash":"AAAA","name":"Foo","category":"movies","tags":"link/movies/Foo, info/year/2020","save_path":"/seed/movies"},
            {"hash":"BBBB","name":"Bar","category":"","tags":"","save_path":"/seed/misc"}
        ]"#;
        let (base, _stop, _h) = spawn_mock(move |req| {
            assert!(
                req.starts_with("GET /api/v2/torrents/info "),
                "request: {req}"
            );
            json_response(json)
        });
        let client = Client::new(&base).unwrap();
        let infos = client
            .torrents_info(&TorrentsInfoQuery::default())
            .await
            .unwrap();
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].hash, "AAAA");
        assert_eq!(infos[0].name, "Foo");
        assert_eq!(infos[0].category.as_deref(), Some("movies"));
        assert_eq!(infos[0].tags, vec!["link/movies/Foo", "info/year/2020"]);
        assert_eq!(infos[0].save_path, "/seed/movies");
        assert_eq!(infos[1].category, None);
        assert!(infos[1].tags.is_empty());
    }

    #[tokio::test]
    async fn torrents_info_sends_filters() {
        let (base, _stop, _h) = spawn_mock(|req| {
            // First line: "GET /api/v2/torrents/info?hashes=AAAA%7CBBBB&category=movies&tag=year HTTP/1.1"
            let first = req.lines().next().unwrap_or("");
            assert!(first.contains("hashes="), "missing hashes in {first}");
            assert!(first.contains("AAAA"), "missing first hash in {first}");
            assert!(first.contains("BBBB"), "missing second hash in {first}");
            // `|` is percent-encoded to %7C by reqwest's url encoder.
            assert!(first.contains("%7C"), "expected %7C separator in {first}");
            assert!(
                first.contains("category=movies"),
                "missing category in {first}"
            );
            assert!(first.contains("tag=year"), "missing tag in {first}");
            json_response("[]")
        });
        let client = Client::new(&base).unwrap();
        let q = TorrentsInfoQuery {
            hashes: vec!["AAAA".into(), "BBBB".into()],
            category: Some("movies".into()),
            tag: Some("year".into()),
        };
        let infos = client.torrents_info(&q).await.unwrap();
        assert!(infos.is_empty());
    }

    #[tokio::test]
    async fn torrents_info_http_error() {
        let (base, _stop, _h) = spawn_mock(|_| {
            let body = "Forbidden";
            format!(
                "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
        });
        let client = Client::new(&base).unwrap();
        let err = client
            .torrents_info(&TorrentsInfoQuery::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::AddFailed { status: 403, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn new_rejects_invalid_url() {
        assert!(matches!(
            Client::new("not a url"),
            Err(Error::InvalidBaseUrl(_))
        ));
    }

    #[test]
    fn new_normalizes_trailing_slash() {
        let c = Client::new("http://127.0.0.1:8080").unwrap();
        assert_eq!(c.base_url().as_str(), "http://127.0.0.1:8080/");
        let c = Client::new("http://127.0.0.1:8080/qbt").unwrap();
        assert_eq!(c.base_url().as_str(), "http://127.0.0.1:8080/qbt/");
    }
}
