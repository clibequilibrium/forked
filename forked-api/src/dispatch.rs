//! A transport-agnostic HTTP dispatcher.
//!
//! Both the self-hosted axum server and the forked cloud Durable Object
//! feed `(method, path, query, body)` through [`dispatch`], so the two
//! deployments expose byte-identical routes, bodies and error shapes by
//! construction, "same API, switch via baseUrl".

use forked_core::{Forked, Storage};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::dto::*;
use crate::error::{ApiError, ApiResult};
use crate::handlers;

/// A borrowed, transport-neutral HTTP request.
#[derive(Debug, Clone, Copy)]
pub struct ApiRequest<'a> {
    /// Uppercase HTTP method (`GET`, `POST`, …).
    pub method: &'a str,
    /// URL path, e.g. `/v1/sessions/abc/branches/main/state`.
    pub path: &'a str,
    /// Raw query string without the `?` (may be empty).
    pub query: &'a str,
    /// Request body bytes (empty for GET).
    pub body: &'a [u8],
}

/// A transport-neutral response: a status code and a JSON body.
#[derive(Debug, Clone)]
pub struct ApiResponse {
    /// HTTP status code.
    pub status: u16,
    /// JSON body (always present; errors use the standard error shape).
    pub body: String,
}

/// Route a request to the matching handler.
///
/// `new_session_id` supplies a generated id for `POST /v1/sessions` when the
/// body does not name one (id generation is transport-specific: uuid
/// natively, Web Crypto on Workers).
pub async fn dispatch<S: Storage>(
    f: &Forked<S>,
    req: ApiRequest<'_>,
    new_session_id: impl FnOnce() -> String,
) -> ApiResponse {
    match route(f, req, new_session_id).await {
        Ok(response) => response,
        Err(e) => ApiResponse {
            status: e.status,
            body: e.body_json(),
        },
    }
}

async fn route<S: Storage>(
    f: &Forked<S>,
    req: ApiRequest<'_>,
    new_session_id: impl FnOnce() -> String,
) -> ApiResult<ApiResponse> {
    let segments: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();

    match (req.method, segments.as_slice()) {
        ("GET", ["v1", "health"]) => ok(&HealthResponse {
            status: "ok".to_string(),
        }),
        ("POST", ["v1", "sessions"]) => {
            let body: CreateSessionRequest = parse_body_or_default(req.body)?;
            let id = body.session_id.unwrap_or_else(new_session_id);
            ok(&handlers::create_session(f, id).await?)
        }
        ("GET", ["v1", "sessions", id, "branches"]) => ok(&handlers::branches(f, id).await?),
        ("GET", ["v1", "sessions", id, "branches", b, "state"]) => {
            ok(&handlers::get_state(f, id, b).await?)
        }
        ("POST", ["v1", "sessions", id, "branches", b, "commit"]) => {
            let body: CommitRequest = parse_body(req.body)?;
            ok(&handlers::commit(f, id, b, body).await?)
        }
        ("POST", ["v1", "sessions", id, "branches", b, "fork"]) => {
            let body: ForkRequest = parse_body(req.body)?;
            ok(&handlers::fork(f, id, b, body).await?)
        }
        ("POST", ["v1", "sessions", id, "branches", b, "checkout"]) => {
            let body: CheckoutRequest = parse_body(req.body)?;
            ok(&handlers::checkout(f, id, b, body).await?)
        }
        ("GET", ["v1", "sessions", id, "log"]) => {
            let branch = query_param(req.query, "branch");
            let branch = branch.as_deref().unwrap_or(forked_core::DEFAULT_BRANCH);
            let limit = match query_param(req.query, "limit") {
                Some(raw) => Some(
                    raw.parse::<usize>()
                        .map_err(|_| ApiError::invalid("limit must be an integer"))?,
                ),
                None => None,
            };
            ok(&handlers::log(f, id, branch, limit).await?)
        }
        ("GET", ["v1", "sessions", _id, "diff"]) => {
            let a = query_param(req.query, "a")
                .ok_or_else(|| ApiError::invalid("missing query parameter: a"))?;
            let b = query_param(req.query, "b")
                .ok_or_else(|| ApiError::invalid("missing query parameter: b"))?;
            ok(&handlers::diff(f, &a, &b).await?)
        }
        ("GET", ["v1", "sessions", id, "graph"]) => ok(&handlers::graph(f, id).await?),
        ("POST", ["v1", "sessions", id, "effects"]) => {
            let body: RecordEffectRequest = parse_body(req.body)?;
            ok(&handlers::record_effect(f, id, body).await?)
        }
        ("GET", ["v1", "sessions", id, "effects"]) => {
            let key = query_param(req.query, "key")
                .ok_or_else(|| ApiError::invalid("missing query parameter: key"))?;
            ok(&handlers::find_effect(f, id, &key).await?)
        }
        (_, ["v1", ..]) => Err(ApiError::new(
            404,
            "unknown_route",
            format!("no route for {} {}", req.method, req.path),
        )),
        _ => Err(ApiError::new(
            404,
            "unknown_route",
            "forked API routes live under /v1",
        )),
    }
}

fn ok<T: Serialize>(value: &T) -> ApiResult<ApiResponse> {
    let body = serde_json::to_string(value)
        .map_err(|e| ApiError::new(500, "internal", format!("response encoding: {e}")))?;
    Ok(ApiResponse { status: 200, body })
}

fn parse_body<T: DeserializeOwned>(body: &[u8]) -> ApiResult<T> {
    serde_json::from_slice(body).map_err(|e| ApiError::invalid(format!("invalid JSON body: {e}")))
}

/// Like [`parse_body`], but an empty body parses as the type's default.
fn parse_body_or_default<T: DeserializeOwned + Default>(body: &[u8]) -> ApiResult<T> {
    if body.is_empty() {
        Ok(T::default())
    } else {
        parse_body(body)
    }
}

/// Extract a query parameter. Values are percent-decoded for `%xx` escapes
/// and `+` (hashes, branch names and limits never need more).
fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == name {
            Some(percent_decode(v))
        } else {
            None
        }
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
