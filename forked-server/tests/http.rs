//! End-to-end HTTP tests through the full axum router (no sockets; tower
//! `oneshot`). These exercise the same dispatcher forked cloud uses.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use forked_core::Forked;
use forked_memory::MemoryStorage;
use forked_server::{AppState, Backend};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn app(token: Option<&str>) -> Router {
    forked_server::router(Arc::new(AppState {
        forked: Forked::new(Backend::Memory(MemoryStorage::new())),
        token: token.map(String::from),
    }))
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    let body = match body {
        Some(v) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn health_and_404() {
    let app = app(None);
    let (status, body) = call(&app, "GET", "/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");

    let (status, body) = call(&app, "GET", "/v1/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "unknown_route");
}

#[tokio::test]
async fn full_session_lifecycle() {
    let app = app(None);

    // Create (server-generated id).
    let (status, created) = call(&app, "POST", "/v1/sessions", Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let sid = created["session_id"].as_str().unwrap().to_string();
    assert_eq!(created["branch"], "main");

    // Commit.
    let (status, committed) = call(
        &app,
        "POST",
        &format!("/v1/sessions/{sid}/branches/main/commit"),
        Some(json!({
            "ops": [
                {"op": "put", "key": "plan", "value": ["a", "b"]},
                {"op": "put", "key": "step", "value": 1}
            ],
            "meta": {"message": "first step"}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{committed}");
    let first_head = committed["head"].as_str().unwrap().to_string();

    // State.
    let (status, state) = call(
        &app,
        "GET",
        &format!("/v1/sessions/{sid}/branches/main/state"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["head"], first_head.as_str());
    assert_eq!(state["state"]["plan"], json!(["a", "b"]));

    // Fork + diverge.
    let (status, forked) = call(
        &app,
        "POST",
        &format!("/v1/sessions/{sid}/branches/main/fork"),
        Some(json!({"branch": "alt"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{forked}");
    let (status, _) = call(
        &app,
        "POST",
        &format!("/v1/sessions/{sid}/branches/alt/commit"),
        Some(json!({"ops": [{"op": "put", "key": "step", "value": 2}]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Branches list.
    let (status, branches) = call(&app, "GET", &format!("/v1/sessions/{sid}/branches"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(branches["branches"].as_array().unwrap().len(), 2);

    // Log with ?branch and ?limit.
    let (status, log) = call(
        &app,
        "GET",
        &format!("/v1/sessions/{sid}/log?branch=alt&limit=1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(log["entries"].as_array().unwrap().len(), 1);
    let alt_head = log["entries"][0]["hash"].as_str().unwrap().to_string();

    // Diff main..alt.
    let (status, diff) = call(
        &app,
        "GET",
        &format!("/v1/sessions/{sid}/diff?a={first_head}&b={alt_head}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(diff["entries"][0]["key"], "step");
    assert_eq!(diff["entries"][0]["change"], "modified");

    // Checkout (rewind alt back to the shared head).
    let (status, _) = call(
        &app,
        "POST",
        &format!("/v1/sessions/{sid}/branches/alt/checkout"),
        Some(json!({"commit": first_head})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, state) = call(
        &app,
        "GET",
        &format!("/v1/sessions/{sid}/branches/alt/state"),
        None,
    )
    .await;
    assert_eq!(state["state"]["step"], 1);

    // Graph sees both branches, the anchor, and all commits.
    let (status, graph) = call(&app, "GET", &format!("/v1/sessions/{sid}/graph"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(graph["branches"].as_array().unwrap().len(), 2);
    assert_eq!(graph["anchors"].as_array().unwrap().len(), 1);
    assert!(graph["commits"].as_array().unwrap().len() >= 3);
}

#[tokio::test]
async fn effects_are_idempotent_over_http() {
    let app = app(None);
    let (_, created) = call(
        &app,
        "POST",
        "/v1/sessions",
        Some(json!({"session_id": "fx"})),
    )
    .await;
    assert_eq!(created["session_id"], "fx");

    let effect = json!({
        "key": "charge-99",
        "kind": "payment.charge",
        "payload": {"amount": 99},
        "result": {"txn": "t-1"}
    });
    let (status, first) = call(
        &app,
        "POST",
        "/v1/sessions/fx/effects",
        Some(effect.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["status"], "recorded");

    let (status, replay) = call(&app, "POST", "/v1/sessions/fx/effects", Some(effect)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["status"], "already_recorded");
    assert_eq!(replay["record"]["result"]["txn"], "t-1");

    // Read-only lookup: found and not-found.
    let (status, found) = call(&app, "GET", "/v1/sessions/fx/effects?key=charge-99", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["record"]["result"]["txn"], "t-1");
    let (status, missing) = call(&app, "GET", "/v1/sessions/fx/effects?key=nope", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(missing["record"].is_null());

    // Same key, different payload: rejected, not replayed.
    let changed = json!({
        "key": "charge-99",
        "kind": "payment.charge",
        "payload": {"amount": 120},
        "result": {"txn": "t-2"}
    });
    let (status, err) = call(&app, "POST", "/v1/sessions/fx/effects", Some(changed)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(err["error"]["code"], "effect_mismatch");
}

#[tokio::test]
async fn conflict_maps_to_409() {
    let app = app(None);
    let (_, created) = call(
        &app,
        "POST",
        "/v1/sessions",
        Some(json!({"session_id": "c"})),
    )
    .await;
    let head = created["head"].as_str().unwrap();

    let commit = |expected: &str| {
        json!({
            "ops": [{"op": "put", "key": "x", "value": 1}],
            "expected_head": expected
        })
    };
    let (status, _) = call(
        &app,
        "POST",
        "/v1/sessions/c/branches/main/commit",
        Some(commit(head)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &app,
        "POST",
        "/v1/sessions/c/branches/main/commit",
        Some(commit(head)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "conflict");
}

#[tokio::test]
async fn bearer_token_is_enforced() {
    let app = app(Some("sekrit"));
    let (status, body) = call(&app, "GET", "/v1/health", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");

    let request = Request::builder()
        .method("GET")
        .uri("/v1/health")
        .header(header::AUTHORIZATION, "Bearer sekrit")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
