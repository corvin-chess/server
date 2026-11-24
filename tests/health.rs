//! CV-130: `/health` must return HTTP 200 with a Compose-compatible body.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn health_returns_200_ok_body() {
    let app = corvin_server::health_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    // Infra Compose probe greps for "ok"; keep the stub body shape.
    assert_eq!(&bytes[..], b"ok\n");
}

#[tokio::test]
async fn unknown_path_returns_404() {
    let app = corvin_server::health_router();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/no-such-route")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("oneshot");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
