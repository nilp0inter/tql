//! Shared axum request-tracing middleware for `tql api` and `tql mcp --http`.
//!
//! Emits one `tracing::info!` event per HTTP request with method, path,
//! response status, and elapsed millis. `/health` is suppressed to keep
//! liveness probes from drowning out useful traffic. Uses the global
//! subscriber installed by `logging::init` (Leg 38).

use std::time::Instant;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

pub async fn trace_request(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let start = Instant::now();
    let resp = next.run(req).await;
    if path != "/health" {
        let status = resp.status().as_u16();
        let elapsed_ms = start.elapsed().as_millis() as u64;
        tracing::info!(
            method = %method,
            path = %path,
            status,
            elapsed_ms,
            "http_request"
        );
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/x", get(|| async { "x" }))
            .layer(axum::middleware::from_fn(trace_request))
    }

    #[tokio::test]
    async fn middleware_does_not_alter_status() {
        let resp = app()
            .oneshot(
                HttpRequest::builder()
                    .uri("/x")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn middleware_passes_health_through() {
        let resp = app()
            .oneshot(
                HttpRequest::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
