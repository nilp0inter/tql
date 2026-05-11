//! Shared HTTP mock helpers for unit tests.
//!
//! Each `spawn_mock` binds an ephemeral 127.0.0.1 port, accepts incoming
//! connections, hands the raw request to a `Fn(&str) -> String` handler, and
//! writes whatever the handler returns. Use [`ok_text`] / [`ok_json`] to
//! produce well-formed HTTP/1.1 responses.

#![cfg(test)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

/// Spawn a minimal HTTP/1.1 mock on a random `127.0.0.1` port.
///
/// Returns `(base_url, stop_flag, join_handle)`. The accept loop reads each
/// request fully (headers + `Content-Length` body), invokes `handler`, and
/// writes the response. The loop exits when `stop_flag` flips or the listener
/// is dropped.
pub fn spawn_mock<F>(handler: F) -> (String, Arc<AtomicBool>, thread::JoinHandle<()>)
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

/// `text/plain` 200 OK with arbitrary extra headers (e.g. `Set-Cookie:`).
/// `extra` is appended verbatim before the blank line; pass `""` for none.
pub fn ok_text(body: &str, extra: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n{}\r\n{}",
        body.len(),
        extra,
        body
    )
}

/// `application/json` 200 OK.
pub fn ok_json(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}
